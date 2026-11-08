//! The request handler: text commands in, status and text out. This is the layer a front end
//! talks to; it checks the token, parses the command and calls the service.
//!
//! ```text
//! LOGIN name password          -> a token
//! WHOAMI                       -> the caller's name and role
//! CREATE title|body|tag,tag    -> the new document id
//! GET id | UPDATE id|body | DELETE id
//! LIST | SEARCH term           -> one line per document
//! EXPORT id format             -> the document as txt, md or json
//! ATTACH name|content | FILE name
//! ```

use crate::auth;
use crate::error::{Result, VaultError};
use crate::export::{self, Format};
use crate::files::FileRoot;
use crate::service::Service;
use crate::store::{DocId, Document};
use crate::text::{escape_display, has_control, parse_tags, split_fields, truncate};
use crate::wire::Frame;

/// A status code and the text that goes with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    fn ok(body: String) -> Response {
        Response { status: 200, body }
    }

    fn from_error(e: &VaultError) -> Response {
        Response { status: e.status(), body: e.message().to_string() }
    }
}

/// The handler: a service, the secret its tokens are signed with, and the attachments folder.
#[derive(Debug)]
pub struct Handler {
    service: Service,
    secret: String,
    files: FileRoot,
}

fn parse_id(s: &str) -> Result<DocId> {
    s.trim().parse().map_err(|_| VaultError::Invalid(format!("{s:?} is not a document id")))
}

fn summary(doc: &Document) -> String {
    format!("{}\t{}\t{} bytes", doc.id, escape_display(&truncate(&doc.title, 60)), doc.size())
}

impl Handler {
    /// A handler over `service`, signing tokens with `secret`.
    pub fn new(service: Service, secret: &str, files: FileRoot) -> Handler {
        Handler { service, secret: secret.to_string(), files }
    }

    /// The service, for reading.
    pub fn service(&self) -> &Service {
        &self.service
    }

    /// The service, for setting things up.
    pub fn service_mut(&mut self) -> &mut Service {
        &mut self.service
    }

    /// Runs one command for the holder of `token` (`LOGIN` needs none).
    pub fn handle(&mut self, token: &str, line: &str) -> Response {
        match self.dispatch(token, line) {
            Ok(body) => Response::ok(body),
            Err(e) => Response::from_error(&e),
        }
    }

    /// Runs the command a binary frame stands for.
    pub fn handle_frame(&mut self, token: &str, bytes: &[u8]) -> Response {
        let line = Frame::decode(bytes).and_then(|(frame, _)| frame.to_line());
        match line {
            Ok(line) => self.handle(token, &line),
            Err(e) => Response::from_error(&e),
        }
    }

    fn dispatch(&mut self, token: &str, line: &str) -> Result<String> {
        if has_control(line) {
            return Err(VaultError::Malformed("a command is one line".into()));
        }
        let line = line.trim();
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        let verb = verb.to_ascii_uppercase();
        if verb == "LOGIN" {
            let mut words = rest.split_whitespace();
            let (Some(name), Some(password)) = (words.next(), words.next()) else {
                return Err(VaultError::Invalid("LOGIN needs a name and a password".into()));
            };
            return auth::login(&self.service.users, &self.secret, name, password);
        }
        let user = auth::verify_token(&self.service.users, &self.secret, token)?;
        match verb.as_str() {
            "WHOAMI" => {
                let u = self.service.users.get(user).ok_or_else(|| VaultError::Unauthenticated("gone".into()))?;
                Ok(format!("{} ({})", u.name, u.role.name()))
            }
            "CREATE" => {
                let f = split_fields(rest, 3);
                let id = self.service.create(user, f[0], f[1], parse_tags(f[2]))?;
                Ok(format!("created {id}"))
            }
            "GET" => {
                let doc = self.service.fetch(user, parse_id(rest)?)?;
                Ok(format!("{}\n\n{}", doc.title, doc.body))
            }
            "UPDATE" => {
                let f = split_fields(rest, 2);
                let id = parse_id(f[0])?;
                self.service.update(user, id, f[1])?;
                Ok(format!("updated {id}"))
            }
            "DELETE" => {
                let id = parse_id(rest)?;
                self.service.delete(user, id)?;
                Ok(format!("deleted {id}"))
            }
            "LIST" => Ok(self.service.list(user).into_iter().map(summary).collect::<Vec<_>>().join("\n")),
            "SEARCH" => Ok(self.service.search(user, rest)?.into_iter().map(summary).collect::<Vec<_>>().join("\n")),
            "EXPORT" => {
                let f: Vec<&str> = rest.split_whitespace().collect();
                let [id, format] = f[..] else {
                    return Err(VaultError::Invalid("EXPORT needs an id and a format".into()));
                };
                let doc = self.service.fetch(user, parse_id(id)?)?;
                Ok(export::render(doc, Format::parse(format)?))
            }
            "ATTACH" => {
                let role = self.service.users.role_of(user);
                if !role.is_some_and(|r| r.can_write()) {
                    return Err(VaultError::Denied("guests cannot attach files".into()));
                }
                let f = split_fields(rest, 2);
                self.files.write(f[0], f[1].as_bytes())?;
                Ok(format!("stored {}", f[0]))
            }
            "FILE" => Ok(String::from_utf8_lossy(&self.files.read(rest)?).into_owned()),
            _ => Err(VaultError::Invalid(format!("unknown command {verb:?}"))),
        }
    }
}
