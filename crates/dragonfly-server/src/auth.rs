use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::collections::HashMap;

use argon2::{password_hash::SaltString, Argon2, PasswordHasher, PasswordVerifier};
use axum::{
    extract::{Form, Extension, Request, State, Query},
    http::{StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    Router,
    routing::{get, post},
    Json,
};
use axum_login::{AuthUser, AuthnBackend};
use rand::{distributions::Alphanumeric, Rng, rngs::OsRng};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{error, info};
use async_trait::async_trait;
use serde_json;
use minijinja::{Error as MiniJinjaError, ErrorKind as MiniJinjaErrorKind};

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
        .route("/login-test", get(login_test_handler))
}

// Remove Askama derives, add Serialize
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
        let demo_user = Admin {
            id: 1,
            username,
            password_hash: "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$WrTpFYXQY6pZu0K+uskWZwl8fOk0W4Dj/pXGXJ9qPXc".to_string(),
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
        Ok(_) => Redirect::to("/login").into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
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

// Add pub to make require_admin public
pub fn require_admin(auth_session: &AuthSession) -> Result<(), Response> {
    // Check if we're in demo mode
    let is_demo_mode = std::env::var("DRAGONFLY_DEMO_MODE").is_ok();
    
    // In demo mode, allow access to admin actions even if not authenticated
    // (should not happen since we force login, but just in case)
    if is_demo_mode && auth_session.user.is_some() {
        return Ok(());
    }
    
    if auth_session.user.is_none() {
        let body = serde_json::json!({ "error": "Unauthorized", "message": "Admin authentication required" });
        Err((StatusCode::UNAUTHORIZED, Json(body)).into_response())
    } else {
        Ok(())
    }
}

// Debug endpoint to verify login status
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