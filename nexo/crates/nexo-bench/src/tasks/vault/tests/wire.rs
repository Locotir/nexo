use vault::error::VaultError;
use vault::wire::*;

#[test]
fn a_frame_survives_the_round_trip() {
    let frame = Frame::new(Kind::Put, vec![1, 2, 3, 250]);
    let bytes = frame.encode().unwrap();
    assert_eq!(bytes.len(), HEADER_LEN + 4 + 1);
    assert_eq!(&bytes[..4], b"VLT1");
    let (back, used) = Frame::decode(&bytes).unwrap();
    assert_eq!((back, used), (frame, bytes.len()));
}

#[test]
fn several_frames_are_read_in_order() {
    let mut buf = Frame::new(Kind::Ping, vec![]).encode().unwrap();
    buf.extend(Frame::new(Kind::Get, 7u64.to_be_bytes().to_vec()).encode().unwrap());
    let frames = Frame::decode_all(&buf).unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1].kind, Kind::Get);
    assert!(Frame::decode_all(&[]).unwrap().is_empty());
}

#[test]
fn damaged_frames_are_refused() {
    let good = Frame::new(Kind::Put, vec![9, 9, 9]).encode().unwrap();
    let mut bad_magic = good.clone();
    bad_magic[0] = b'X';
    let mut bad_sum = good.clone();
    *bad_sum.last_mut().unwrap() ^= 1;
    let mut bad_kind = good.clone();
    bad_kind[4] = 77;
    assert!(Frame::decode(&bad_magic).is_err());
    assert!(matches!(Frame::decode(&bad_sum), Err(VaultError::Malformed(_))));
    assert!(Frame::decode(&bad_kind).is_err());
    assert!(Frame::decode(&good[..5]).is_err());
    assert!(Frame::decode(&[]).is_err());
}

#[test]
fn big_payloads_are_not_encoded() {
    assert!(Frame::new(Kind::Put, vec![0; MAX_PAYLOAD + 1]).encode().is_err());
    assert!(Frame::new(Kind::Put, vec![0; MAX_PAYLOAD]).encode().is_ok());
}

#[test]
fn a_put_payload_carries_title_and_body() {
    let payload = encode_put("Plan", "the body").unwrap();
    assert_eq!(parse_put(&payload).unwrap(), ("Plan".to_string(), "the body".to_string()));
    assert_eq!(parse_put(&encode_put("", "").unwrap()).unwrap(), (String::new(), String::new()));
}

#[test]
fn a_put_payload_that_lies_about_its_title_is_refused() {
    assert!(parse_put(&[]).is_err());
    assert!(parse_put(&[0]).is_err());
    assert!(parse_put(&[0xff, 0xff, b'a']).is_err());
    assert!(parse_put(&[0, 3, b'a', b'b']).is_err());
    assert!(parse_put(&[0, 1, 0xff]).is_err());
}

#[test]
fn ids_have_eight_bytes() {
    assert_eq!(parse_id(&42u64.to_be_bytes()).unwrap(), 42);
    assert!(parse_id(&[1, 2, 3]).is_err());
    assert!(parse_id(&[0; 9]).is_err());
}

#[test]
fn frames_turn_into_commands() {
    let put = Frame::new(Kind::Put, encode_put("Plan", "the body").unwrap());
    assert_eq!(put.to_line().unwrap(), "CREATE Plan|the body|");
    assert_eq!(Frame::new(Kind::Ping, vec![]).to_line().unwrap(), "WHOAMI");
    assert_eq!(Frame::new(Kind::Get, 5u64.to_be_bytes().to_vec()).to_line().unwrap(), "GET 5");
    assert_eq!(Frame::new(Kind::Delete, 5u64.to_be_bytes().to_vec()).to_line().unwrap(), "DELETE 5");
    assert!(Frame::new(Kind::Put, encode_put("a|b", "x").unwrap()).to_line().is_err());
    assert!(Frame::new(Kind::Get, vec![1]).to_line().is_err());
}
