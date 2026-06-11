use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("no stored credentials")]
    NotFound,
    #[error("keyring error: {0}")]
    Keyring(String),
}

const SERVICE: &str = "com.remnantfinder.drive";
const USER_TOKEN: &str = "api_token";
const USER_COMPANY: &str = "company_id";

pub fn save_token(token: &str) -> Result<(), AuthError> {
    keyring::Entry::new(SERVICE, USER_TOKEN)
        .map_err(|e| AuthError::Keyring(e.to_string()))?
        .set_password(token)
        .map_err(|e| AuthError::Keyring(e.to_string()))
}

pub fn load_token() -> Result<String, AuthError> {
    keyring::Entry::new(SERVICE, USER_TOKEN)
        .map_err(|e| AuthError::Keyring(e.to_string()))?
        .get_password()
        .map_err(|_| AuthError::NotFound)
}

pub fn save_company_id(company_id: &str) -> Result<(), AuthError> {
    keyring::Entry::new(SERVICE, USER_COMPANY)
        .map_err(|e| AuthError::Keyring(e.to_string()))?
        .set_password(company_id)
        .map_err(|e| AuthError::Keyring(e.to_string()))
}

pub fn load_company_id() -> Result<String, AuthError> {
    keyring::Entry::new(SERVICE, USER_COMPANY)
        .map_err(|e| AuthError::Keyring(e.to_string()))?
        .get_password()
        .map_err(|_| AuthError::NotFound)
}

pub fn clear_credentials() -> Result<(), AuthError> {
    let _ = keyring::Entry::new(SERVICE, USER_TOKEN)
        .map_err(|e| AuthError::Keyring(e.to_string()))?
        .delete_credential();
    let _ = keyring::Entry::new(SERVICE, USER_COMPANY)
        .map_err(|e| AuthError::Keyring(e.to_string()))?
        .delete_credential();
    Ok(())
}
