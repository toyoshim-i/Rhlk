use rhlk::format::obj::parse_object;
use rhlk::format::FormatError;

#[test]
fn rejects_unsupported_command() {
    let data = [0x12, 0x34];
    let err = parse_object(&data).expect_err("parser must reject unknown commands");
    assert!(matches!(err, FormatError::UnsupportedCommand(0x1234)));
}
