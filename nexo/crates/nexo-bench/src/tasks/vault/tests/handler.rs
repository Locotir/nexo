mod common;

use common::{handler, login, Scratch};

#[test]
fn login_gives_a_token_and_whoami_names_it() {
    let scratch = Scratch::new("handler");
    let mut h = handler(&scratch);
    let token = login(&mut h, "alice");
    let r = h.handle(&token, "whoami");
    assert_eq!((r.status, r.body.as_str()), (200, "alice (member)"));
    assert_eq!(h.handle("", "LOGIN alice wrong-pass1").status, 401);
    assert_eq!(h.handle("", "LOGIN alice").status, 400);
}

#[test]
fn a_request_without_a_valid_token_is_refused() {
    let scratch = Scratch::new("handler");
    let mut h = handler(&scratch);
    assert_eq!(h.handle("", "LIST").status, 401);
    assert_eq!(h.handle("2.0000000000000000", "LIST").status, 401);
    assert_eq!(h.handle("junk", "WHOAMI").status, 401);
}

#[test]
fn documents_go_through_create_get_update_list_and_delete() {
    let scratch = Scratch::new("handler");
    let mut h = handler(&scratch);
    let t = login(&mut h, "alice");
    assert_eq!(h.handle(&t, "CREATE Plan | the body | work, Q3").body, "created 1");
    assert_eq!(h.handle(&t, "GET 1").body, "Plan\n\nthe body");
    assert_eq!(h.handle(&t, "UPDATE 1|new body").body, "updated 1");
    assert_eq!(h.handle(&t, "GET 1").body, "Plan\n\nnew body");
    assert_eq!(h.handle(&t, "LIST").body, "1\tPlan\t8 bytes");
    assert_eq!(h.handle(&t, "DELETE 1").body, "deleted 1");
    assert_eq!(h.handle(&t, "GET 1").status, 404);
    assert_eq!(h.service().store.len(), 0);
}

#[test]
fn another_user_cannot_get_or_update_a_document() {
    let scratch = Scratch::new("handler");
    let mut h = handler(&scratch);
    let (a, b, root) = (login(&mut h, "alice"), login(&mut h, "bob"), login(&mut h, "root"));
    h.handle(&a, "CREATE Plan|secret|");
    assert_eq!(h.handle(&b, "GET 1").status, 403);
    assert_eq!(h.handle(&b, "UPDATE 1|mine").status, 403);
    assert_eq!(h.handle(&root, "GET 1").status, 200);
    assert_eq!(h.handle(&root, "DELETE 1").status, 200);
}

#[test]
fn list_escapes_titles_for_the_page() {
    let scratch = Scratch::new("handler");
    let mut h = handler(&scratch);
    let t = login(&mut h, "alice");
    h.handle(&t, "CREATE <script>alert(1)</script>|x|");
    assert_eq!(h.handle(&t, "LIST").body, "1\t&lt;script&gt;alert(1)&lt;/script&gt;\t1 bytes");
}

#[test]
fn search_and_export_work_on_the_callers_documents() {
    let scratch = Scratch::new("handler");
    let mut h = handler(&scratch);
    let (a, b) = (login(&mut h, "alice"), login(&mut h, "bob"));
    h.handle(&a, "CREATE Budget 2026|numbers|finance");
    h.handle(&b, "CREATE Budget draft|other|");
    assert_eq!(h.handle(&a, "SEARCH budget").body, "1\tBudget 2026\t7 bytes");
    assert_eq!(h.handle(&a, "SEARCH ").status, 400);
    assert_eq!(h.handle(&a, "EXPORT 1 md").body, "# Budget 2026\n\nnumbers\n\n`finance`\n");
    assert_eq!(h.handle(&a, "EXPORT 1").status, 400);
    assert_eq!(h.handle(&a, "EXPORT 1 exe").status, 400);
    assert_eq!(h.handle(&a, "EXPORT 2 md").status, 403);
}

#[test]
fn attachments_are_stored_and_served() {
    let scratch = Scratch::new("handler");
    let mut h = handler(&scratch);
    let (a, g) = (login(&mut h, "alice"), login(&mut h, "gary"));
    assert_eq!(h.handle(&a, "ATTACH notes/todo.txt|buy milk").body, "stored notes/todo.txt");
    assert_eq!(h.handle(&g, "FILE notes/todo.txt").body, "buy milk");
    assert_eq!(h.handle(&g, "ATTACH x.txt|nope").status, 403);
    assert_eq!(h.handle(&a, "FILE missing.txt").status, 404);
    assert_eq!(h.handle("", "FILE notes/todo.txt").status, 401);
}

#[test]
fn guests_read_but_do_not_write() {
    let scratch = Scratch::new("handler");
    let mut h = handler(&scratch);
    let g = login(&mut h, "gary");
    assert_eq!(h.handle(&g, "CREATE Hi|x|").status, 403);
    assert_eq!(h.handle(&g, "LIST").body, "");
}

#[test]
fn odd_commands_get_a_clean_error() {
    let scratch = Scratch::new("handler");
    let mut h = handler(&scratch);
    let t = login(&mut h, "alice");
    assert_eq!(h.handle(&t, "FLY away").status, 400);
    assert_eq!(h.handle(&t, "").status, 400);
    assert_eq!(h.handle(&t, "GET one").status, 400);
    assert_eq!(h.handle(&t, "GET 1\nDELETE 1").status, 400);
    assert_eq!(h.handle(&t, "UPDATE 99|x").status, 404);
}

#[test]
fn frames_drive_the_same_commands() {
    use vault::wire::{encode_put, Frame, Kind};
    let scratch = Scratch::new("handler");
    let mut h = handler(&scratch);
    let t = login(&mut h, "alice");
    let put = Frame::new(Kind::Put, encode_put("Framed", "via bytes").unwrap()).encode().unwrap();
    assert_eq!(h.handle_frame(&t, &put).body, "created 1");
    let get = Frame::new(Kind::Get, 1u64.to_be_bytes().to_vec()).encode().unwrap();
    assert_eq!(h.handle_frame(&t, &get).body, "Framed\n\nvia bytes");
    assert_eq!(h.handle_frame(&t, &Frame::new(Kind::Ping, vec![]).encode().unwrap()).status, 200);
    assert_eq!(h.handle_frame(&t, b"garbage").status, 400);
    assert_eq!(h.handle_frame("", &get).status, 401);
}
