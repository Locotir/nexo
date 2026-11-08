//! Users, their roles and the directory that looks them up.

use crate::auth::hash_password;
use crate::error::{Result, VaultError};
use crate::text::is_valid_username;

/// The id of a user; ids start at 1 and are never reused.
pub type UserId = u32;

/// What a user may do. The order matters: a higher role can do everything a lower one can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// Reads what is shared with them, writes nothing.
    Guest,
    /// Keeps documents of their own.
    Member,
    /// Manages everybody's documents.
    Admin,
}

impl Role {
    /// The role named `s`, if any.
    pub fn parse(s: &str) -> Option<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "guest" => Some(Role::Guest),
            "member" => Some(Role::Member),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }

    /// The name `parse` reads back.
    pub fn name(self) -> &'static str {
        match self {
            Role::Guest => "guest",
            Role::Member => "member",
            Role::Admin => "admin",
        }
    }

    /// Whether the role may create and change documents.
    pub fn can_write(self) -> bool {
        self >= Role::Member
    }
}

/// One account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: UserId,
    pub name: String,
    pub role: Role,
    pub salt: String,
    pub password_hash: u64,
}

/// All the accounts, looked up by id or by name.
#[derive(Debug)]
pub struct Directory {
    users: Vec<User>,
    next: UserId,
}

impl Default for Directory {
    fn default() -> Self {
        Directory::new()
    }
}

impl Directory {
    /// An empty directory.
    pub fn new() -> Directory {
        Directory { users: Vec::new(), next: 1 }
    }

    /// Adds an account. Names are unique ignoring case; passwords need 8 characters.
    pub fn add(&mut self, name: &str, role: Role, password: &str) -> Result<UserId> {
        if !is_valid_username(name) {
            return Err(VaultError::Invalid(format!("bad user name {name:?}")));
        }
        if password.chars().count() < 8 {
            return Err(VaultError::Invalid("the password needs at least 8 characters".into()));
        }
        if self.by_name(name).is_some() {
            return Err(VaultError::Invalid(format!("the name {name} is taken")));
        }
        let id = self.next;
        self.next += 1;
        let salt = format!("vault:{id}:{}", name.to_ascii_lowercase());
        let password_hash = hash_password(&salt, password);
        self.users.push(User { id, name: name.to_string(), role, salt, password_hash });
        Ok(id)
    }

    /// The account with this id.
    pub fn get(&self, id: UserId) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }

    /// The account with this name, ignoring case.
    pub fn by_name(&self, name: &str) -> Option<&User> {
        self.users.iter().find(|u| u.name.eq_ignore_ascii_case(name))
    }

    /// The role of a user, if the user exists.
    pub fn role_of(&self, id: UserId) -> Option<Role> {
        self.get(id).map(|u| u.role)
    }

    /// Whether the user exists and is an admin.
    pub fn is_admin(&self, id: UserId) -> bool {
        self.role_of(id) == Some(Role::Admin)
    }

    /// Changes a user's role. The last admin cannot be demoted.
    pub fn set_role(&mut self, id: UserId, role: Role) -> Result<()> {
        let admins = self.users.iter().filter(|u| u.role == Role::Admin).count();
        let user =
            self.users.iter_mut().find(|u| u.id == id).ok_or_else(|| VaultError::NotFound(format!("user {id}")))?;
        if user.role == Role::Admin && role != Role::Admin && admins == 1 {
            return Err(VaultError::Invalid("the last admin cannot be demoted".into()));
        }
        user.role = role;
        Ok(())
    }

    /// How many accounts there are.
    pub fn len(&self) -> usize {
        self.users.len()
    }

    /// Whether there are no accounts.
    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    /// The names of all accounts, in creation order.
    pub fn names(&self) -> Vec<&str> {
        self.users.iter().map(|u| u.name.as_str()).collect()
    }
}
