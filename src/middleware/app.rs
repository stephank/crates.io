use axum::extract::State;
use axum::middleware::Next;
use axum::response::Response;
use http::Request;

use crate::app::AppState;

/// `axum` middleware that injects the `AppState` instance into the `Request` extensions.
pub async fn add_app_state_extension<B>(
    State(app_state): State<AppState>,
    mut request: Request<B>,
    next: Next<B>,
) -> Response {
    request.extensions_mut().insert(app_state);

    next.run(request).await
}

/// Adds an `app()` method to the `Request` type returning the global `App` instance
pub trait RequestApp {
    fn app(&self) -> &AppState;
}

impl<T> RequestApp for Request<T> {
    fn app(&self) -> &AppState {
        self.extensions().get::<AppState>().expect("Missing app")
    }
}
