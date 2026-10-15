//! Auth error algebra. `Display` strings are French and meant to be shown as-is in the UI.
//! Public `AuthManager` methods return `anyhow::Result`; callers can `downcast_ref::<AuthError>()`.

/// Errors of the `auth` module. Never carries secrets (keys, tokens, codes, certificates).
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Serveurs Ankama injoignables, vérifiez votre connexion internet")]
    Network(String),
    #[error("Trop de requêtes envoyées à Ankama, réessayez dans quelques minutes")]
    RateLimited,
    #[error("Connexion refusée par Ankama : VPN ou proxy détecté. Désactivez-le puis réessayez")]
    AnonymousIpForbidden,
    #[error("Session expirée, reconnectez-vous")]
    SessionExpired,
    #[error("Accès refusé par Ankama pour ce compte, reconnectez-vous ou réessayez plus tard")]
    AccountErrored,
    #[error("{}", haapi_message(*status, reason.as_deref()))]
    Haapi { status: u16, reason: Option<String> },
    #[error("Réponse inattendue des serveurs Ankama ({0})")]
    InvalidResponse(String),
    #[error("Connexion refusée par Ankama ({0})")]
    OAuth(String),
    #[error("Délai de connexion dépassé, réessayez")]
    LoginTimeout,
    #[error("Connexion annulée")]
    LoginCancelled,
    #[error("Le port 9001 est déjà utilisé (Ankama Launcher ouvert ?), fermez-le puis réessayez")]
    PortBusy,
    #[error("Compte introuvable")]
    UnknownAccount(i64),
    #[error("Aucune validation de sécurité en attente pour ce compte, reconnectez-vous")]
    NoPendingShield,
    #[error("Adresse e-mail non confirmée : validez-la depuis le mail reçu d'Ankama")]
    EmailNotValidated,
    #[error("Erreur de stockage local : {0}")]
    Storage(String),
    #[error("Erreur de chiffrement : {0}")]
    Crypto(String),
}

impl AuthError {
    /// Haapi `reason`/`message` field, if any.
    pub fn reason(&self) -> Option<&str> {
        match self {
            AuthError::Haapi { reason, .. } => reason.as_deref(),
            _ => None,
        }
    }
    /// HTTP status for Haapi errors.
    pub fn status(&self) -> Option<u16> {
        match self {
            AuthError::Haapi { status, .. } => Some(*status),
            AuthError::RateLimited => Some(429),
            _ => None,
        }
    }
}

impl From<std::io::Error> for AuthError {
    fn from(e: std::io::Error) -> Self {
        AuthError::Storage(e.to_string())
    }
}

/// French UI text for the Haapi error vocabulary (spec 01 section 1).
fn haapi_message(status: u16, reason: Option<&str>) -> String {
    let text = match reason.unwrap_or("") {
        "BAN" => "Ce compte est banni",
        "BLACKLIST" => "Ce compte est sur liste noire",
        "LOCKED" => "Ce compte est verrouillé",
        "DELETED" => "Ce compte a été supprimé",
        "RESETANKAMA" => "Le mot de passe de ce compte doit être réinitialisé",
        "OTPTIMEFAILED" => "Code de l'authentificateur refusé : vérifiez l'heure de votre téléphone",
        "SECURITYCARD" => "Carte de sécurité requise",
        "BRUTEFORCE" => "Trop de tentatives, réessayez plus tard",
        "FAILED" => "Échec de l'authentification",
        "MAILNOVALID" => "Adresse e-mail non confirmée",
        "NOACCOUNT" => "Compte inexistant",
        "ACCOUNT_INVALID" => "Compte invalide",
        "ACCOUNT_SHIELDED" => "Compte protégé par Shield : validation requise",
        "ACCOUNT_NO_CERTIFY" => "Appareil non certifié : validation Shield requise",
        "CODEBADCODE" => "Code de sécurité incorrect",
        "CODEEXPIRE" => "Code de sécurité expiré, demandez-en un nouveau",
        "CODENOTFOUND" => "Code de sécurité introuvable, demandez-en un nouveau",
        "CODEPROBLEM" => "Problème avec le code de sécurité, demandez-en un nouveau",
        "ALREADYSECURED" => "Cet appareil est déjà sécurisé, reconnectez-vous",
        _ => "",
    };
    if !text.is_empty() {
        return text.to_string();
    }
    match reason {
        Some(r) if !r.is_empty() => format!("Erreur Ankama ({status}) : {r}"),
        _ => format!("Erreur Ankama ({status})"),
    }
}
