//! Endpoints for yanking and unyanking specific versions of crates

use crate::auth::AuthCheck;

use super::version_and_crate;
use crate::controllers::cargo_prelude::*;
use crate::models::token::EndpointScope;
use crate::models::Rights;
use crate::models::{insert_version_owner_action, VersionAction};
use crate::schema::versions;
use crate::worker;

/// Handles the `DELETE /crates/:crate_id/:version/yank` route.
/// This does not delete a crate version, it makes the crate
/// version accessible only to crates that already have a
/// `Cargo.lock` containing this version.
///
/// Notes:
/// Crate deletion is not implemented to avoid breaking builds,
/// and the goal of yanking a crate is to prevent crates
/// beginning to depend on the yanked crate version.
pub async fn yank(
    Path((crate_name, version)): Path<(String, String)>,
    req: ConduitRequest,
) -> AppResult<Response> {
    conduit_compat(move || modify_yank(&crate_name, &version, &req, true)).await
}

/// Handles the `PUT /crates/:crate_id/:version/unyank` route.
pub async fn unyank(
    Path((crate_name, version)): Path<(String, String)>,
    req: ConduitRequest,
) -> AppResult<Response> {
    conduit_compat(move || modify_yank(&crate_name, &version, &req, false)).await
}

/// Changes `yanked` flag on a crate version record
fn modify_yank<B>(
    crate_name: &str,
    version: &str,
    req: &Request<B>,
    yanked: bool,
) -> AppResult<Response> {
    // FIXME: Should reject bad requests before authentication, but can't due to
    // lifetime issues with `req`.

    if semver::Version::parse(version).is_err() {
        return Err(cargo_err(&format_args!("invalid semver: {version}")));
    }

    let auth = AuthCheck::default()
        .with_endpoint_scope(EndpointScope::Yank)
        .for_crate(crate_name)
        .check(req)?;

    let state = req.app();
    let conn = state.db_write()?;
    let (version, krate) = version_and_crate(&conn, crate_name, version)?;
    let api_token_id = auth.api_token_id();
    let user = auth.user();
    let owners = krate.owners(&conn)?;

    if user.rights(state, &owners)? < Rights::Publish {
        return Err(cargo_err("must already be an owner to yank or unyank"));
    }

    if version.yanked == yanked {
        // The crate is already in the state requested, nothing to do
        return ok_true();
    }

    diesel::update(&version)
        .set(versions::yanked.eq(yanked))
        .execute(&*conn)?;

    let action = if yanked {
        VersionAction::Yank
    } else {
        VersionAction::Unyank
    };

    insert_version_owner_action(&conn, version.id, user.id, api_token_id, action)?;

    worker::sync_yanked(krate.name, version.num).enqueue(&conn)?;

    ok_true()
}
