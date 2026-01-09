# web-core

Core utils for Axum projects

This is a work in progress project.

```toml
[dependencies]
web-core = { git = "https://github.com/Salman-Sali/web-core.git", tag = "0.1.0" }

# when you run in local or dev environment
# Right now it will enable cors allow all
[features]
local = ["web-core/local"]


# for when running tests.
[dev-dependencies]
web-core = { version = "0.1.0", path = "../web-core", features = ["test_mode"] }
```

```rust
#[tokio::main]
async fn main() -> Result<(), Error> {
    let options = AuthOptions::new(
        String::from("<my secret here>"), 
        Duration::from_secs(60 * 60),//access token lifetime 
        Duration::from_secs(30 * 24 * 60 * 60)//refresh token lifetime
    ).with_audience(String::from("<my url here>"));

    let my_app_state = MyAppState::default();
    let web_core_state = WebCoreState::new(AuthService::new(options), my_app_state);

    let protected_routes = Router::new()
        ...
        .with_auth_layer(web_core_state.auth_service.clone());

    let web_core_options = WebCoreOptions::new(web_core_state)
        .with_frontend_url(String::from("<my url here>"));

    let open_routes = Router::new()...;

    let app = Router::new().merge(open_routes).merge(protected_routes).with_web_core(web_core_options);

    run(app().await).await
}

pub fn get_protected_routes() -> Router<WebCoreState<AppState>> {
    Router::new()
        .nest("/posts", Router::new()
            .route("/", post(create_post))
    )        
}

pub async fn create_post(
    state: State<WebCoreState<AppState>>, // access your app state, auth_service, etc from here
    Extension(user): Extension<AuthenticatedUser>,//get subject from token here
    ...
) -> Result<Json<CreatePostResponse>, ApiError> {
    ...
    ...
    ...
}

// you can also find useful macros like: 
something_went_wrong!();
unauthorized!();
bad_request!();
not_found!();
```
