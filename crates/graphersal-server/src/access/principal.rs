//! Who sends a request: the [`Principal`] the authenticator chain produced.

/// The role that gives full rights: every database read-write, the admin API, Runs, Users.
pub const ADMIN_ROLE: &str = "admin";

/// What kind of principal sent a request.
///
/// `#[non_exhaustive]`: later authenticators (JWT) may add kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PrincipalKind {
    /// The console on this machine, with the operator token of this run (`server.run`): full
    /// rights.
    Operator,
    /// A user of `access.toml`.
    User,
    /// A request without credentials: the anonymous grants only.
    Anonymous,
}

impl PrincipalKind {
    /// The kind as the API names it: `operator`, `user`, `anonymous`.
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalKind::Operator => "operator",
            PrincipalKind::User => "user",
            PrincipalKind::Anonymous => "anonymous",
        }
    }
}

/// The principal of a request: its id, kind, roles and groups.
///
/// `#[non_exhaustive]`: build one with [`Principal::operator`], [`Principal::user`] or
/// [`Principal::anonymous`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Principal {
    /// The id: the user's name, `operator` or `anonymous`.
    pub id: String,
    /// The kind.
    pub kind: PrincipalKind,
    /// The roles (`admin`); empty for the operator and anonymous.
    pub roles: Vec<String>,
    /// The groups (none yet: a later authenticator (JWT claims) fills them).
    pub groups: Vec<String>,
}

impl Principal {
    /// The operator (the console with this run's operator token).
    pub fn operator() -> Self {
        Self {
            id: "operator".to_string(),
            kind: PrincipalKind::Operator,
            roles: Vec::new(),
            groups: Vec::new(),
        }
    }

    /// The user `name` with `roles`.
    pub fn user(name: impl Into<String>, roles: Vec<String>) -> Self {
        Self {
            id: name.into(),
            kind: PrincipalKind::User,
            roles,
            groups: Vec::new(),
        }
    }

    /// The anonymous principal.
    pub fn anonymous() -> Self {
        Self {
            id: "anonymous".to_string(),
            kind: PrincipalKind::Anonymous,
            roles: Vec::new(),
            groups: Vec::new(),
        }
    }

    /// Whether it has full rights: the operator, or a principal with the `admin` role.
    pub fn is_admin(&self) -> bool {
        self.kind == PrincipalKind::Operator || self.roles.iter().any(|r| r == ADMIN_ROLE)
    }

    /// Whether it is the anonymous principal.
    pub fn is_anonymous(&self) -> bool {
        self.kind == PrincipalKind::Anonymous
    }

    /// The identity handed to graphersal's hosting crates (`Request::set_identity`, MCP
    /// sessions): `user:<name>`, `operator`; `None` for anonymous (graphersal treats a request
    /// without identity as anonymous, so no user name can collide with it).
    pub fn identity(&self) -> Option<String> {
        match self.kind {
            PrincipalKind::Operator => Some("operator".to_string()),
            PrincipalKind::User => Some(format!("user:{}", self.id)),
            PrincipalKind::Anonymous => None,
        }
    }

    /// The grant subjects it holds, as `access.toml` writes them: `user:<name>`, `role:<r>`,
    /// `group:<g>`, and `anonymous` for everyone (what anonymous may see, a logged-in principal
    /// may see too).
    pub fn subjects(&self) -> Vec<String> {
        let mut subjects = Vec::new();
        if self.kind == PrincipalKind::User {
            subjects.push(format!("user:{}", self.id));
        }
        subjects.extend(self.roles.iter().map(|r| format!("role:{r}")));
        subjects.extend(self.groups.iter().map(|g| format!("group:{g}")));
        subjects.push("anonymous".to_string());
        subjects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_and_subjects() {
        let alice = Principal::user("alice", vec!["admin".to_string()]);
        assert_eq!(alice.identity().as_deref(), Some("user:alice"));
        assert!(alice.is_admin());
        assert_eq!(alice.subjects(), ["user:alice", "role:admin", "anonymous"]);
        let bob = Principal::user("bob", Vec::new());
        assert!(!bob.is_admin());
        assert_eq!(
            Principal::operator().identity().as_deref(),
            Some("operator")
        );
        assert!(Principal::operator().is_admin());
        let anonymous = Principal::anonymous();
        assert_eq!(anonymous.identity(), None);
        assert!(anonymous.is_anonymous() && !anonymous.is_admin());
        assert_eq!(anonymous.subjects(), ["anonymous"]);
    }
}
