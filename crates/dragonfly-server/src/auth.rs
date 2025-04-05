use anyhow::Context;
use async_session::{MemoryStore, Session, SessionStore};
use async_trait::async_trait;
use axum::{
    extract::{FromRef, FromRequestParts, Path, Query, State},
    http::{header, request::Parts, HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response, Html},
    routing::{get, post},
    Extension, Form,
    Router,
};
use axum_extra::extract::{cookie::Key, SignedCookieJar};
use base64::engine::Engine;
use oauth2::{
    basic::BasicClient,
    reqwest::async_http_client,
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use std::env;
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info, warn};
use argon2::{
    password_hash::{PasswordHash, PasswordVerifier as ArgonPasswordVerifier, SaltString},
    Argon2, PasswordHasher,
};
use rand::rngs::OsRng;
use minijinja::{Error as MiniJinjaError, ErrorKind as MiniJinjaErrorKind};
use axum_login::{AuthUser, AuthnBackend, UserId};
use std::io;
use std::fs;
use std::collections::HashMap;
use std::path::Path as StdPath;
use rand::{Rng, distributions::Alphanumeric};
use crate::ui::AddAlert;
use thiserror::Error;
use std::sync::Arc;
use urlencoding;

use crate::{
    ui::AlertMessage,
    AppState,
};

// Constants for the initial password file (not for loading, just for UX)
const INITIAL_PASSWORD_FILE: &str = "initial_password.txt";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub password_hash: String,
}

impl Default for Credentials {
    fn default() -> Self {
        Self {
            username: "admin".to_string(),
            password: None,
            password_hash: String::new(),
        }
    }
}

impl Credentials {
    pub fn create(username: String, password: String) -> io::Result<Self> {
        let salt = SaltString::generate(&mut OsRng);
        
        let password_hash = match Argon2::default().hash_password(password.as_bytes(), &salt) {
            Ok(hash) => hash.to_string(),
            Err(e) => {
                return Err(io::Error::new(io::ErrorKind::Other, format!("Failed to hash password: {}", e)));
            }
        };
        
        Ok(Self {
            username,
            password: None, // Don't store plaintext password
            password_hash,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AdminUser {
    pub id: i64,
    pub username: String,
}

impl AuthUser for AdminUser {
    type Id = i64;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn session_auth_hash(&self) -> &[u8] {
        self.username.as_bytes()
    }
}

// Define a basic AuthError for OAuth callback
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("OAuth state mismatch")]
    StateMismatch,
    #[error("Missing query parameter: {0}")]
    MissingParam(String),
    #[error("Cookie error: {0}")]
    CookieError(String),
    #[error("OAuth token exchange failed: {0}")]
    TokenExchangeFailed(String),
    #[error("Failed to get user info: {0}")]
    UserInfoFailed(String),
    #[error("Session login error: {0}")]
    SessionLoginError(String),
    #[error(transparent)]
    RequestTokenError(#[from] oauth2::RequestTokenError<oauth2::reqwest::Error<reqwest::Error>, oauth2::StandardErrorResponse<oauth2::basic::BasicErrorResponseType>>),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// Implement IntoResponse for AuthError to return appropriate HTTP responses
impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        error!("Authentication error: {}", self);
        // Redirect to login with an error message
        Redirect::to(&format!("/login?error={}", urlencoding::encode(&self.to_string())))
            .into_response()
            .add_alert(AlertMessage::error(&self.to_string())) // Use AddAlert here
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub require_login: bool,
    pub default_os: Option<String>,
    pub setup_completed: bool,
    pub admin_username: String,
    pub admin_password_hash: String,
    pub admin_email: String,
    pub oauth_enabled: bool,
    pub oauth_provider: Option<String>,
    pub oauth_client_id: Option<String>,
    pub oauth_client_secret: Option<String>,
    
    // Add the missing Proxmox fields
    pub proxmox_host: Option<String>,
    pub proxmox_username: Option<String>,
    pub proxmox_password: Option<String>,
    pub proxmox_port: Option<u16>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            require_login: false,
            default_os: None,
            setup_completed: false,
            admin_username: "admin".to_string(),
            admin_password_hash: String::new(), // Default to empty, should be set
            admin_email: String::new(),
            oauth_enabled: false,
            oauth_provider: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            proxmox_host: None,
            proxmox_username: None,
            proxmox_password: None,
            proxmox_port: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AdminBackend {
    db: sqlx::SqlitePool,
    settings: Settings,
}

impl AdminBackend {
    pub fn new(db: sqlx::SqlitePool, settings: Settings) -> Self {
        Self { db, settings }
    }
    
    pub async fn update_credentials(&self, username: String, password: String) -> anyhow::Result<Credentials> {
        // Create new credentials with hashed password
        let new_credentials = Credentials::create(username, password)?;
        
        // Save to database
        save_credentials(&new_credentials).await?;
        
        Ok(new_credentials)
    }
}

impl Default for AdminBackend {
    fn default() -> Self {
        // NOTE: This default implementation likely won't work without a valid DB connection.
        // Consider removing it or making it connect to an in-memory DB for tests.
        // For now, let it panic if the connection fails.
        let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite::memory:".to_string());
        let pool = tokio::runtime::Runtime::new().unwrap().block_on(async {
            sqlx::sqlite::SqlitePoolOptions::new()
                .connect(&db_url)
                .await
        }).expect("Failed to create SQLite pool in AdminBackend::default");

        Self {
            db: pool,
            settings: Settings::default(),
        }
    }
}

#[async_trait]
impl AuthnBackend for AdminBackend {
    type User = AdminUser;
    type Credentials = Credentials;
    type Error = MiniJinjaError;

    async fn authenticate(
        &self,
        creds: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        let username = creds.username.clone();

        // Fetch the stored hash from the database
        let stored_hash = match sqlx::query!(
            "SELECT password_hash FROM admin_credentials WHERE username = ?",
            creds.username
        )
        .fetch_optional(&self.db)
        .await
        {
            Ok(Some(record)) => record.password_hash,
            Ok(None) => {
                info!("Authentication failed: User '{}' not found", creds.username);
                return Ok(None);
            }
            Err(e) => {
                error!("Database error fetching credentials for user '{}': {}", creds.username, e);
                return Err(MiniJinjaError::new(MiniJinjaErrorKind::InvalidOperation, format!("Database error: {}", e)));
            }
        };

        // Convert password to owned bytes Option
        let password_bytes = creds.password.map(|p| p.into_bytes());

        // Verify the password using Argon2 within a blocking task
        let is_valid = match password_bytes {
            Some(bytes) => {
                // Move stored_hash and bytes into the closure
                match tokio::task::spawn_blocking(move || {
                    match PasswordHash::new(&stored_hash) {
                        Ok(parsed_hash) => {
                            Argon2::default().verify_password(&bytes, &parsed_hash).is_ok()
                        }
                        Err(_) => false // Error parsing hash means invalid
                    }
                }).await {
                    Ok(result) => result,
                    Err(e) => {
                        error!("Spawn blocking task failed during password verification for user '{}': {}", username, e);
                        false // Treat join errors as verification failure
                    }
                }
            }
            None => {
                info!("Authentication failed for user '{}': No password provided", username);
                false // No password provided
            }
        };

        if is_valid {
            info!("Authentication successful for user '{}'", username);
            // Fetch the user ID from the database to create the AdminUser
             match sqlx::query_as!(AdminUser, "SELECT id, username FROM admin_credentials WHERE username = ?", username)
                .fetch_one(&self.db)
                .await {
                    Ok(user) => Ok(Some(user)),
                    Err(e) => {
                        error!("Database error fetching user details for '{}': {}", username, e);
                         Err(MiniJinjaError::new(MiniJinjaErrorKind::InvalidOperation, format!("Database error fetching user: {}", e)))
                    }
                }
        } else {
            info!("Authentication failed: Invalid password for user '{}'", username);
            Ok(None)
        }
    }

    async fn get_user(&self, user_id: &UserId<Self>) -> Result<Option<Self::User>, Self::Error> {
        // Fetch the user from the database based on the user_id
        match sqlx::query_as!(
            AdminUser,
            "SELECT id, username FROM admin_credentials WHERE id = ?",
            user_id
        )
        .fetch_optional(&self.db)
        .await
        {
            Ok(user_opt) => Ok(user_opt),
            Err(e) => {
                error!("Database error fetching user by ID '{}': {}", user_id, e);
                 Err(MiniJinjaError::new(MiniJinjaErrorKind::InvalidOperation, format!("Database error fetching user by ID: {}", e)))
            }
        }
    }
}

pub type AuthSession = axum_login::AuthSession<AdminBackend>;

pub fn auth_router() -> Router<crate::AppState> {
    Router::new()
        .route("/login", get(login_page))
        .route("/login", post(login_handler))
        .route("/logout", post(logout))
        .route("/login-test", get(login_test_handler))
}

#[derive(Serialize)]
struct LoginTemplate {
    is_demo_mode: bool,
    error: Option<String>,
}

async fn login_page(
    State(app_state): State<crate::AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    // Check if we're in demo mode
    let is_demo_mode = std::env::var("DRAGONFLY_DEMO_MODE").is_ok();
    
    // Check for error parameter
    let error = params.get("error").cloned();
    if let Some(err) = &error {
        info!("Login page loaded with error: {}", err);
    }
    
    let template = LoginTemplate {
        is_demo_mode,
        error,
    };
    
    // Get the environment based on the mode (static or reloading)
    let render_result = match &app_state.template_env {
        crate::TemplateEnv::Static(env) => {
            env.get_template("login.html")
               .and_then(|tmpl| tmpl.render(&template))
        }
        #[cfg(debug_assertions)]
        crate::TemplateEnv::Reloading(reloader) => {
            // Acquire the environment from the reloader
            match reloader.acquire_env() {
                Ok(env) => {
                    env.get_template("login.html")
                       .and_then(|tmpl| tmpl.render(&template))
                }
                Err(e) => {
                    error!("Failed to acquire MiniJinja env from reloader: {}", e);
                    Err(MiniJinjaError::new(MiniJinjaErrorKind::InvalidOperation, 
                        format!("Failed to acquire env from reloader: {}", e)))
                }
            }
        }
    };

    // Handle the final rendering result
    match render_result {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!("MiniJinja render/load error for login.html: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response()
        }
    }
}

async fn login_handler(
    mut auth_session: AuthSession,
    Form(form): Form<LoginForm>,
) -> Response {
    // Check if we're in demo mode
    let is_demo_mode = std::env::var("DRAGONFLY_DEMO_MODE").is_ok();
    
    if is_demo_mode {
        // In demo mode, simply create a demo user and force-login without authentication
        info!("Demo mode: accepting any credentials for login");
        
        // Create a simple admin user 
        let username = if form.username.trim().is_empty() { "demo_user".to_string() } else { form.username.clone() };
        
        // Create a demo admin user - use the same hash as lib.rs creates for demo credentials
        let demo_user = AdminUser {
            id: 1,
            username,
        };
        
        // Hard-set the user session
        info!("Demo mode: Setting session for user '{}'", demo_user.username);
        match auth_session.login(&demo_user).await {
            Ok(_) => {
                info!("Demo mode: Login successful for user '{}'", demo_user.username);
                return Redirect::to("/").into_response();
            },
            Err(e) => {
                error!("Demo mode: Failed to set user session: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR, 
                    "Internal error setting demo session"
                ).into_response();
            }
        }
    }
    
    // Regular authentication flow for non-demo mode
    info!("Processing login request for user '{}'", form.username);
    
    let credentials = Credentials {
        username: form.username.clone(),
        password: Some(form.password),
        password_hash: String::new(),
    };
    
    // Try to authenticate the user
    match auth_session.authenticate(credentials).await {
        Ok(Some(user)) => {
            // Successfully authenticated, set up the session
            if let Err(e) = auth_session.login(&user).await {
                error!("Failed to create session after successful auth: {}", e);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            
            info!("Login successful for user '{}'", user.username);
            Redirect::to("/").into_response()
        }
        Ok(None) => {
            info!("Authentication failed for user '{}'", form.username);
            Redirect::to("/login?error=invalid_credentials").into_response()
        }
        Err(e) => {
            error!("Error during authentication: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn logout(mut auth_session: AuthSession) -> Response {
    match auth_session.logout().await {
        Ok(_) => Redirect::to("/login")
            .into_response()
            .add_alert(AlertMessage::success("Successfully logged out.")),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR
            .into_response()
            .add_alert(AlertMessage::error("Failed to log out.")),
    }
}

pub async fn generate_default_credentials() -> anyhow::Result<Credentials> {
    // Check if an initial password file already exists
    if StdPath::new(INITIAL_PASSWORD_FILE).exists() {
        info!("Initial password file exists - attempting to load existing credentials from database");
        // Try to load credentials from database first
        if let Ok(Some(creds)) = crate::db::get_admin_credentials().await {
            info!("Found existing admin credentials in database - using those");
            return Ok(creds);
        } else {
            // If we can't load from database but file exists, we should delete the file
            // as it's probably stale/outdated
            info!("Failed to load admin credentials from database but initial password file exists - file may be stale");
            if let Err(e) = fs::remove_file(INITIAL_PASSWORD_FILE) {
                error!("Failed to remove stale initial password file: {}", e);
            }
        }
    }

    info!("Generating new admin credentials");
    let username = "admin".to_string();
    let password: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect();

    // Create new credentials with proper error handling
    let credentials = Credentials::create(username, password.clone())
        .map_err(|e| anyhow::anyhow!("Failed to create admin credentials: {}", e))?;
    
    // Save to database
    if let Err(e) = crate::db::save_admin_credentials(&credentials).await {
        error!("Failed to save admin credentials to database: {}", e);
        return Err(anyhow::anyhow!("Failed to save admin credentials to database: {}", e));
    }
    
    // Save password to file for user convenience
    if let Err(e) = fs::write(INITIAL_PASSWORD_FILE, &password) {
        error!("Failed to save initial password to file: {}", e);
        // This is not a critical error, so we can continue
    } else {
        info!("Initial admin password saved to {}", INITIAL_PASSWORD_FILE);
    }
    
    info!("Generated default admin credentials. Username: admin, Password: {}", password);
    Ok(credentials)
}

pub async fn load_credentials() -> io::Result<Credentials> {
    // Load only from database - no fallback to file credential loading
    match crate::db::get_admin_credentials().await {
        Ok(Some(creds)) => {
            info!("Loaded admin credentials from database");
            Ok(creds)
        },
        Ok(None) => {
            info!("No admin credentials found in database");
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "No admin credentials found in database",
            ))
        },
        Err(e) => {
            error!("Error loading admin credentials from database: {}", e);
            Err(io::Error::new(
                io::ErrorKind::Other,
                format!("Database error: {}", e),
            ))
        }
    }
}

pub async fn save_credentials(credentials: &Credentials) -> io::Result<()> {
    // Save to database only
    if let Err(e) = crate::db::save_admin_credentials(credentials).await {
        error!("Failed to save admin credentials to database: {}", e);
        return Err(io::Error::new(io::ErrorKind::Other, format!("Database error: {}", e)));
    }
    
    info!("Saved admin credentials to database");
    Ok(())
}

pub async fn load_settings() -> io::Result<Settings> {
    match crate::db::get_app_settings().await {
        Ok(settings) => {
            info!("Loaded settings from database");
            Ok(settings)
        },
        Err(e) => {
            error!("Failed to load settings from database: {}", e);
            Ok(Settings::default()) // Return default settings on error
        }
    }
}

pub async fn save_settings(settings: &Settings) -> io::Result<()> {
    match crate::db::save_app_settings(settings).await {
        Ok(_) => {
            info!("Settings saved to database");
            Ok(())
        },
        Err(e) => {
            error!("Failed to save settings to database: {}", e);
            Err(io::Error::new(io::ErrorKind::Other, format!("Database error: {}", e)))
        }
    }
}

pub fn require_admin(auth_session: &AuthSession) -> Result<(), Response> {
    match auth_session.user {
        Some(_) => Ok(()),
        None => Err(Redirect::to("/login").into_response()),
    }
}

async fn login_test_handler(auth_session: AuthSession) -> impl IntoResponse {
    let is_demo_mode = std::env::var("DRAGONFLY_DEMO_MODE").is_ok();
    let is_authenticated = auth_session.user.is_some();
    
    let username = auth_session.user
        .as_ref()
        .map(|user| user.username.clone())
        .unwrap_or_else(|| "Not logged in".to_string());
    
    let html = format!(
        r#"<!DOCTYPE html>
        <html>
        <head>
            <title>Login Test</title>
            <style>
                body {{ font-family: Arial, sans-serif; padding: 2rem; }}
                .container {{ max-width: 800px; margin: 0 auto; }}
                .panel {{ background-color: #f5f5f5; padding: 1rem; border-radius: 0.5rem; margin-bottom: 1rem; }}
                .demo {{ background-color: #fff3cd; }}
                h1 {{ color: #333; }}
                .label {{ font-weight: bold; margin-right: 0.5rem; }}
                .success {{ color: green; }}
                .error {{ color: red; }}
            </style>
        </head>
        <body>
            <div class="container">
                <h1>Login Test Page</h1>
                
                <div class="panel {demo_class}">
                    <div><span class="label">Demo Mode:</span> {is_demo}</div>
                    <div><span class="label">Session Status:</span> 
                         <span class="{auth_class}">{is_auth}</span>
                    </div>
                    <div><span class="label">Username:</span> {username}</div>
                </div>
                
                <div>
                    <a href="/">Go to Dashboard</a> | 
                    <a href="/login">Go to Login</a>
                </div>
            </div>
        </body>
        </html>
        "#,
        demo_class = if is_demo_mode { "demo" } else { "" },
        is_demo = if is_demo_mode { "Enabled" } else { "Disabled" },
        is_auth = if is_authenticated { "Authenticated" } else { "Not Authenticated" },
        auth_class = if is_authenticated { "success" } else { "error" },
        username = username
    );
    
    Html(html)
}

pub async fn oauth_callback(
    State(app_state): State<AppState>,
    State(client): State<BasicClient>,
    Query(params): Query<HashMap<String, String>>,
    jar: SignedCookieJar,
    mut auth_session: AuthSession,
) -> Result<impl IntoResponse, AuthError> {
    info!("Handling OAuth callback");

    // Verify state parameter
    let state_cookie = jar.get("oauth_state").ok_or(AuthError::CookieError("Missing state cookie".to_string()))?;
    let expected_state = state_cookie.value().to_string();
    let received_state = params.get("state").ok_or(AuthError::MissingParam("state".to_string()))?;

    if expected_state != *received_state {
        return Err(AuthError::StateMismatch);
    }

    // Get PKCE verifier from cookie
    let pkce_verifier_cookie = jar.get("oauth_pkce_verifier").ok_or(AuthError::CookieError("Missing PKCE verifier cookie".to_string()))?;
    let pkce_verifier = PkceCodeVerifier::new(pkce_verifier_cookie.value().to_string());

    // Get authorization code
    let code = params.get("code").ok_or(AuthError::MissingParam("code".to_string()))?;

    // Get settings directly from the passed-in app_state
    let settings = app_state.settings.lock().await;

    // Exchange authorization code for tokens
    let token_result = client
        .exchange_code(AuthorizationCode::new(code.to_string()))
        .set_pkce_verifier(pkce_verifier)
        .request_async(oauth2::reqwest::async_http_client)
        .await
        .map_err(|e| AuthError::TokenExchangeFailed(e.to_string()))?;
        //.map_err(AuthError::RequestTokenError)?;

    // Here you would typically fetch user info using the access token
    // let user_info = fetch_user_info(token_result.access_token()).await?;
    
    // For now, let's just create a placeholder user
    let user = AdminUser {
        id: 1, // Or generate a unique ID based on OAuth provider info
        username: "oauth_user".to_string(), // Use actual username from provider
    };

    // Log the user into the session (use the extracted auth_session)
    // let mut auth_session = AuthSession::from_request_parts(&mut Default::default(), &app_state)
    //     .await
    //     .map_err(|_| AuthError::SessionLoginError("Failed to extract auth session".to_string()))?;
        
    auth_session.login(&user).await.map_err(|e| AuthError::SessionLoginError(format!("Session login failed: {}", e)))?;

    info!("OAuth login successful for user: {}", user.username);

    // Remove used cookies
    let jar = jar.remove(cookie::Cookie::named("oauth_state"));
    let jar = jar.remove(cookie::Cookie::named("oauth_pkce_verifier"));

    // Redirect to the main page
    Ok((jar, Redirect::to("/")).into_response())
}

pub async fn login(
    State(_app_state): State<AppState>, // Mark as unused for now
    mut _auth_session: AuthSession, // Mark as unused for now
    Form(_creds): Form<Credentials>, // Mark as unused for now
) -> Response {
    // Placeholder implementation - This function likely needs to call
    // auth_session.authenticate and auth_session.login similar to login_handler
    // For now, return an error or redirect
    warn!("/api/login endpoint hit, but not fully implemented yet");
    (StatusCode::NOT_IMPLEMENTED, "Login endpoint not fully implemented").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn setup_test_app() -> (Router, AppState) {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .expect("Failed to connect to in-memory SQLite DB");

        // Apply migrations (if you have them)
        // sqlx::migrate!("./migrations").run(&pool).await.expect("Failed migrations");

        // Create dummy settings using default and hash a password
        let mut settings = Settings::default();
        settings.admin_password_hash = hash_password("password".to_string()).await.unwrap();

        // Insert the test user credentials into the DB
        sqlx::query!(
            "INSERT OR IGNORE INTO admin_credentials (username, password_hash) VALUES (?, ?)",
            settings.admin_username,
            settings.admin_password_hash
        )
        .execute(&pool)
        .await
        .expect("Failed to insert test admin credentials");

        // Fetch the ID of the inserted user (or assume 1 if IGNORE worked)
        // let user_record = sqlx::query!("SELECT id FROM admin_credentials WHERE username = ?", settings.admin_username)
        //    .fetch_one(&pool).await.expect("Failed to fetch test user ID");
        // let test_user_id = user_record.id;


        let backend = AdminBackend { db: pool.clone(), settings: settings.clone() };
        let session_store = MemoryStore::new();
        let secret = Key::generate();
        let session_layer = SessionManagerLayer::new(session_store)
            .with_secure(false)
            .with_signed(secret.clone());

        let auth_layer = AuthManagerLayerBuilder::new(backend.clone(), session_layer).build();

        // Create AppState with necessary components
        let app_state = AppState {
            dbpool: pool.clone(),
            settings: backend.settings.clone(),
            template_env: crate::TemplateEnv::Static(Arc::new(crate::ui::create_jinja_env().unwrap())), // Example static env
            event_manager: crate::event_manager::EventManager::new(), // Example event manager
             // auth_backend: backend, // If AppState needs the backend directly
        };

        let app = Router::new()
             // Use the actual login handler route
            .route("/login", post(login_handler))
            .route("/logout", get(logout))
            .route("/protected", get(login_test_handler))
            .layer(auth_layer)
            .with_state(app_state.clone());

        (app, app_state)
    }

    // Dummy handler for protected route test
    async fn login_test_handler(auth_session: AuthSession) -> impl IntoResponse {
        if auth_session.user.is_some() {
            (StatusCode::OK, "Protected content")
        } else {
            (StatusCode::UNAUTHORIZED, "Unauthorized")
        }
    }

    #[tokio::test]
    async fn test_login_success() {
        let (app, _) = setup_test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("username=admin&password=password"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(response.headers().get("location").unwrap().to_str().unwrap().contains("/"));
    }

    #[tokio::test]
    async fn test_login_failure_wrong_password() {
        let (app, _) = setup_test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("username=admin&password=wrongpassword"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(response.headers().get("location").unwrap().to_str().unwrap().contains("/login"));
    }

    #[tokio::test]
    async fn test_login_failure_user_not_found() {
        let (app, _) = setup_test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("username=unknownuser&password=password"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(response.headers().get("location").unwrap().to_str().unwrap().contains("/login"));
    }
}