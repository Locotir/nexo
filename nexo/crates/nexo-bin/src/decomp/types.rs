//! C prototypes and small struct layouts the person gives, kept in the names file.

use std::collections::HashMap;

use crate::names::UserNames;
use crate::Image;

#[derive(Clone, Debug, PartialEq)]
pub struct Proto {
    pub ret: String,
    pub name: String,
    /// (type, name)
    pub args: Vec<(String, String)>,
    pub variadic: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub off: u64,
    pub ty: String,
    pub name: String,
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Split `unsigned char *buf` into (`unsigned char *`, `buf`).
fn split_decl(d: &str) -> (String, String) {
    let d = d.trim();
    let end = d.len();
    let start = d.rfind(|c: char| !is_ident(c)).map_or(0, |i| i + 1);
    let name = &d[start..end];
    let ty = d[..start].trim();
    if ty.is_empty() || name.is_empty() {
        // `int` alone: a type with no name
        return (d.to_string(), String::new());
    }
    (ty.to_string(), name.to_string())
}

pub fn parse_proto(text: &str) -> Result<Proto, String> {
    let t = text.trim().trim_end_matches(';').trim();
    let open = t.find('(').ok_or("a prototype looks like: int name(char *path, int flags)")?;
    let close = t.rfind(')').ok_or("the prototype has no closing )")?;
    if close < open {
        return Err("the prototype has no closing )".into());
    }
    let (ret, name) = split_decl(&t[..open]);
    if name.is_empty() || ret.is_empty() {
        return Err("a prototype needs a return type and a name: int name(...)".into());
    }
    let inner = t[open + 1..close].trim();
    let mut args = Vec::new();
    let mut variadic = false;
    if !inner.is_empty() && inner != "void" {
        for (i, a) in inner.split(',').enumerate() {
            let a = a.trim();
            if a == "..." {
                variadic = true;
                continue;
            }
            let (ty, n) = split_decl(a);
            let n = if n.is_empty() || matches!(ty.as_str(), "struct" | "unsigned" | "const") {
                format!("a{}", i + 1)
            } else {
                n
            };
            let ty = if ty.is_empty() { a.to_string() } else { ty };
            args.push((ty, n));
        }
    }
    Ok(Proto { ret, name, args, variadic })
}

/// `0:i32:id 8:ptr:next` or one field per line.
pub fn parse_struct(text: &str) -> Result<Vec<Field>, String> {
    let mut out = Vec::new();
    for item in text.split(|c: char| c.is_whitespace() || c == ';' || c == ',').filter(|s| !s.is_empty()) {
        let parts: Vec<&str> = item.splitn(3, ':').collect();
        let [off, ty, name] = parts.as_slice() else {
            return Err(format!("{item:?} is not offset:type:name (like 8:ptr:next)"));
        };
        let off = crate::parse_addr(off).map_err(|e| e.to_string())?;
        if name.is_empty() || !name.chars().all(is_ident) {
            return Err(format!("{name:?} is not a field name"));
        }
        out.push(Field { off, ty: (*ty).to_string(), name: (*name).to_string() });
    }
    if out.is_empty() {
        return Err("a struct needs fields: offset:type:name ...".into());
    }
    Ok(out)
}

pub fn protos(user: &UserNames, _img: &Image) -> HashMap<u64, Proto> {
    user.protos.iter().filter_map(|(a, t)| parse_proto(t).ok().map(|p| (*a, p))).collect()
}

pub fn structs(user: &UserNames) -> HashMap<String, Vec<Field>> {
    user.structs.iter().filter_map(|(n, t)| parse_struct(t).ok().map(|f| (n.clone(), f))).collect()
}

/// `struct item *` or `item *` -> `item`.
pub fn struct_of(ty: &str) -> Option<String> {
    if !ty.contains('*') {
        return None;
    }
    let t = ty.replace('*', " ");
    let w: Vec<&str> = t.split_whitespace().filter(|x| !matches!(*x, "struct" | "const" | "volatile")).collect();
    match w.as_slice() {
        [one] => Some((*one).to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototype_and_struct_parse() {
        let p = parse_proto("int load(const char *path, struct item *it, ...)").unwrap();
        assert_eq!(p.ret, "int");
        assert_eq!(p.name, "load");
        assert_eq!(
            p.args,
            vec![("const char *".to_string(), "path".to_string()), ("struct item *".to_string(), "it".to_string())]
        );
        assert!(p.variadic);
        assert_eq!(struct_of("struct item *").as_deref(), Some("item"));
        assert!(parse_proto("nothing").is_err());
        let f = parse_struct("0:i32:id 8:ptr:next").unwrap();
        assert_eq!(f[1], Field { off: 8, ty: "ptr".into(), name: "next".into() });
        assert!(parse_struct("8-next").is_err());
    }
}
