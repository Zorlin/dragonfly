use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::collections::HashMap;

use argon2::{password_hash::SaltString, Argon2, PasswordHasher, PasswordVerifier};
use axum::{
    extract::{Form, Extension, FromRequest, Request},
    http::StatusCode,
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    Router,
    routing::{get, post},
};
use axum_login::{AuthUser, AuthnBackend};
use rand::{distributions::Alphanumeric, Rng, rngs::OsRng};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{error, info};
use async_trait::async_trait;
use askama::Template;

// Constants for the initial password file (not for loading, just for UX)
const INITIAL_PASSWORD_FILE: &str = "initial_password.txt";

// Constants for auth system
const USER_ID_KEY: &str = "user_id";
const SESSION_COOKIE_NAME: &str = "dragonfly_auth";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Admin {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub require_login: bool,
    pub default_os: Option<String>,
    pub setup_completed: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            require_login: false,
            default_os: None,
            setup_completed: false,
        }
    }
}

// The auth user type which will be stored in the session
impl AuthUser for Admin {
    type Id = i64;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn session_auth_hash(&self) -> &[u8] {
        self.password_hash.as_bytes()
    }
}

// Define a backend for authentication
#[derive(Clone)]
pub struct AdminBackend {
    pub credentials: Credentials,
}

impl AdminBackend {
    pub fn new(credentials: Credentials) -> Self {
        Self { credentials }
    }
    
    // Verify credentials
    pub async fn verify_credentials(&self, credentials: Credentials) -> bool {
        // If username doesn't match, reject
        if credentials.username != self.credentials.username {
            return false;
        }
        
        // Verify the password against the stored hash
        let password_matches = match &credentials.password {
            Some(password) => {
                match argon2::Argon2::default().verify_password(
                    password.as_bytes(),
                    &argon2::PasswordHash::new(&self.credentials.password_hash).unwrap(),
                ) {
                    Ok(_) => true,
                    Err(_) => false,
                }
            },
            None => false,
        };
        
        password_matches
    }
    
    // Update credentials
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
        Self {
            credentials: Credentials::default(),
        }
    }
}

// The backend for handling authentication
#[async_trait]
impl AuthnBackend for AdminBackend {
    type User = Admin;
    type Credentials = Credentials;
    type Error = io::Error;

    async fn authenticate(
        &self,
        creds: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        // If username doesn't match, reject
        if creds.username != self.credentials.username {
            return Ok(None);
        }
        
        // Verify the password against the stored hash
        let password_matches = match &creds.password {
            Some(password) => {
                match argon2::Argon2::default().verify_password(
                    password.as_bytes(),
                    &argon2::PasswordHash::new(&self.credentials.password_hash).unwrap(),
                ) {
                    Ok(_) => true,
                    Err(_) => false,
                }
            },
            None => false,
        };
        
        if password_matches {
            Ok(Some(Admin {
                id: 1,
                username: self.credentials.username.clone(),
                password_hash: self.credentials.password_hash.clone(),
            }))
        } else {
            Ok(None)
        }
    }

    async fn get_user(&self, user_id: &i64) -> Result<Option<Self::User>, Self::Error> {
        if *user_id == 1 {
            Ok(Some(Admin {
                id: 1,
                username: self.credentials.username.clone(),
                password_hash: self.credentials.password_hash.clone(),
            }))
        } else {
            Ok(None)
        }
    }
}

// Session types
pub type AuthSession = axum_login::AuthSession<AdminBackend>;

// Setup the auth layer and router
pub fn auth_router() -> Router<crate::AppState> {
    Router::new()
        .route("/login", get(login_page))
        .route("/login", post(login_handler))
        .route("/logout", post(logout))
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {}

async fn login_page() -> impl IntoResponse {
    let template = LoginTemplate {};
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            error!("Template rendering error: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn login_handler(
    mut auth_session: AuthSession,
    Form(form): Form<LoginForm>,
) -> Response {
    let credentials = Credentials {
        username: form.username,
        password: Some(form.password),
        password_hash: String::new(), // This will be ignored during authentication
    };

    let user = match auth_session.authenticate(credentials).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            return Redirect::to("/login?error=invalid_credentials").into_response();
        }
        Err(_) => {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    if auth_session.login(&user).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    Redirect::to("/").into_response()
}

async fn logout(mut auth_session: AuthSession) -> Response {
    match auth_session.logout().await {
        Ok(_) => Redirect::to("/login").into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// Authentication middleware
pub async fn auth_middleware(
    auth_session: AuthSession,
    settings: Extension<Arc<Mutex<Settings>>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    
    // Always allow access to static files and authentication-related routes
    // regardless of settings
    if path.starts_with("/js/") || 
       path.starts_with("/css/") || 
       path.starts_with("/images/") || 
       path == "/login" || 
       path == "/logout" {
        return next.run(req).await;
    }
    
    // Always allow access to API endpoints (they have their own auth checks)
    if path.starts_with("/api/") {
        return next.run(req).await;
    }
    
    // Check if login is required site-wide
    let require_login = {
        let settings_guard = settings.lock().await;
        settings_guard.require_login
    };
    
    if require_login {
        // When login is required, check authentication for ALL other paths
        if auth_session.user.is_none() {
            info!("Auth required for path: {}, redirecting to login", path);
            return Redirect::to("/login").into_response();
        }
    }
    
    // User is authenticated or login not required, proceed
    next.run(req).await
}

// Helper functions for managing credentials
pub async fn generate_default_credentials() -> anyhow::Result<Credentials> {
    // Check if an initial password file already exists
    if Path::new(INITIAL_PASSWORD_FILE).exists() {
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

// Define how credentials are converted from a Form submission
impl<S> FromRequest<S> for Credentials
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request(req: Request<axum::body::Body>, state: &S) -> Result<Self, Self::Rejection> {
        let Form(map) = Form::<HashMap<String, String>>::from_request(req, state).await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        
        Ok(Credentials {
            username: map.get("username").cloned().unwrap_or_default(),
            password: map.get("password").cloned(),
            password_hash: "".to_string(), // This will be set during authentication
        })
    }
} 