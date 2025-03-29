use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use argon2::{password_hash::SaltString, Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::{
    extract::{Form, Extension},
    http::{Request, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    Router,
    routing::{get, post},
};
use axum_extra::extract::cookie::CookieJar;
use axum_login::{AuthUser, AuthnBackend};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use async_trait::async_trait;
use askama::Template;

// Constants for the initial password file (not for loading, just for UX)
const INITIAL_PASSWORD_FILE: &str = "initial_password.txt";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub password_hash: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub require_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            require_login: false,
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

// The backend for handling authentication
#[derive(Debug, Clone)]
pub struct AdminBackend {
    credentials: Arc<Mutex<Credentials>>,
}

impl AdminBackend {
    pub fn new(credentials: Credentials) -> Self {
        Self {
            credentials: Arc::new(Mutex::new(credentials)),
        }
    }

    pub async fn update_credentials(&self, username: String, password: String) -> io::Result<()> {
        let salt = SaltString::generate(rand::thread_rng());
        let password_hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            .to_string();

        let mut creds = self.credentials.lock().await;
        creds.username = username;
        creds.password = None; // Clear any password that might be in memory
        creds.password_hash = password_hash;
        
        // Save directly to database
        save_credentials(&creds).await
    }
    
    pub async fn verify_credentials(&self, creds: Credentials) -> Result<bool, io::Error> {
        let stored_creds = self.credentials.lock().await;
        
        if creds.username != stored_creds.username {
            return Ok(false);
        }

        // Get password from credentials
        let password = match creds.password {
            Some(password) => password,
            None => return Ok(false),
        };

        let is_valid = match PasswordHash::new(&stored_creds.password_hash) {
            Ok(parsed_hash) => Argon2::default()
                .verify_password(password.as_bytes(), &parsed_hash)
                .map_or(false, |_| true),
            Err(_) => false,
        };

        Ok(is_valid)
    }
}

#[async_trait]
impl AuthnBackend for AdminBackend {
    type User = Admin;
    type Credentials = Credentials;
    type Error = io::Error;

    async fn authenticate(
        &self,
        creds: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        let stored_creds = self.credentials.lock().await;
        
        if creds.username != stored_creds.username {
            return Ok(None);
        }

        // Get password from credentials
        let password = match creds.password {
            Some(password) => password,
            None => return Ok(None),
        };

        let is_valid = match PasswordHash::new(&stored_creds.password_hash) {
            Ok(parsed_hash) => Argon2::default()
                .verify_password(password.as_bytes(), &parsed_hash)
                .map_or(false, |_| true),
            Err(_) => false,
        };

        if is_valid {
            Ok(Some(Admin {
                id: 1, // Only one admin for now
                username: stored_creds.username.clone(),
                password_hash: stored_creds.password_hash.clone(),
            }))
        } else {
            Ok(None)
        }
    }

    async fn get_user(&self, id: &i64) -> Result<Option<Self::User>, Self::Error> {
        if *id == 1 {
            let creds = self.credentials.lock().await;
            Ok(Some(Admin {
                id: 1,
                username: creds.username.clone(),
                password_hash: creds.password_hash.clone(),
            }))
        } else {
            Ok(None)
        }
    }
}

// Session types
pub type AuthSession = axum_login::AuthSession<AdminBackend>;

// Setup the auth layer and router
pub fn auth_router() -> Router {
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
    _jar: CookieJar,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    
    // Always allow access to:
    // 1. Static files and login/logout routes
    // 2. API endpoints (they have their own auth checks)
    // 3. Settings page (restricted features handled in the handler)
    // 4. Machine registration endpoints (needed for initial setup)
    if path.starts_with("/js/") || 
       path.starts_with("/css/") || 
       path.starts_with("/images/") || 
       path == "/login" || 
       path == "/logout" ||
       path == "/settings" ||
       path.starts_with("/api/") {
        return next.run(req).await;
    }
    
    // Check if login is required
    let require_login = {
        let settings_guard = settings.lock().await;
        settings_guard.require_login
    };
    
    if require_login {
        // Check if user is authenticated
        if auth_session.user.is_none() {
            // Redirect to login page
            return Redirect::to("/login").into_response();
        }
    }
    
    // User is authenticated or login not required, proceed
    next.run(req).await
}

// Helper functions for managing credentials
pub fn generate_default_credentials() -> Credentials {
    let username = "admin".to_string();
    let password: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect();

    let salt = SaltString::generate(rand::thread_rng());
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("Failed to hash password")
        .to_string();

    let credentials = Credentials {
        username,
        password: None, // We don't store the password in the credentials
        password_hash,
    };

    // Save initial password to a file for better UX
    // This way admins can access it later if needed
    let password_message = format!(
        "# Dragonfly Initial Admin Password\n\n\
        This is your initial admin password. For security, change it after logging in.\n\n\
        Username: admin\n\
        Password: {}\n\n\
        Delete this file after you've securely saved the password elsewhere.", 
        password
    );
    
    if let Err(e) = fs::write(INITIAL_PASSWORD_FILE, password_message) {
        error!("Failed to save initial password to file: {}", e);
    } else {
        info!("Initial admin password saved to {}", INITIAL_PASSWORD_FILE);
    }

    // Save credentials to database - but in a separate task to avoid blocking
    let credentials_clone = credentials.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::db::save_admin_credentials(&credentials_clone).await {
            error!("Failed to save admin credentials to database: {}", e);
        } else {
            info!("Successfully saved admin credentials to database");
        }
    });

    info!("Generated default admin credentials. Username: admin, Password: {}", password);
    credentials
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

pub fn load_settings() -> Settings {
    let path = Path::new("settings.json");
    if !path.exists() {
        let default_settings = Settings::default();
        if let Err(e) = save_settings(&default_settings) {
            error!("Failed to save default settings: {}", e);
        }
        return default_settings;
    }

    match fs::read_to_string(path) {
        Ok(data) => match serde_json::from_str(&data) {
            Ok(settings) => settings,
            Err(e) => {
                error!("Failed to parse settings file: {}", e);
                Settings::default()
            }
        },
        Err(e) => {
            error!("Failed to read settings file: {}", e);
            Settings::default()
        }
    }
}

pub fn save_settings(settings: &Settings) -> io::Result<()> {
    let data = serde_json::to_string_pretty(settings)?;
    let mut file = File::create("settings.json")?;
    file.write_all(data.as_bytes())?;
    Ok(())
} 