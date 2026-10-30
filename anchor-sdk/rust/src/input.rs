//! Wire helpers for the `org.anchor.input@1` capability.

use crate::v1::{self, CapabilityAdvertisement, EndpointAdvertisement};
use prost::Message;

pub const ENDPOINT_ID: &str = "io.anchor.desktop";
pub const CAPABILITY_NAME: &str = "org.anchor.input";
pub const CAPABILITY_MAJOR: u32 = 1;
pub const KEY_TYPE_URL: &str = "type.googleapis.com/anchor.v1.capabilities.input.InputKey";
pub const TEXT_TYPE_URL: &str = "type.googleapis.com/anchor.v1.capabilities.input.InputText";
pub const POINTER_ABSOLUTE_TYPE_URL: &str =
    "type.googleapis.com/anchor.v1.capabilities.input.InputPointerAbsolute";
pub const POINTER_RELATIVE_TYPE_URL: &str =
    "type.googleapis.com/anchor.v1.capabilities.input.InputPointerRelative";
pub const POINTER_BUTTON_TYPE_URL: &str =
    "type.googleapis.com/anchor.v1.capabilities.input.InputPointerButton";
pub const SCROLL_TYPE_URL: &str = "type.googleapis.com/anchor.v1.capabilities.input.InputScroll";
pub const REMOTE_CONTROL_TYPE_URL: &str =
    "type.googleapis.com/anchor.v1.capabilities.input.RemoteControl";
pub const PING_TYPE_URL: &str = "type.googleapis.com/anchor.v1.capabilities.input.InputPing";
pub const PONG_TYPE_URL: &str = "type.googleapis.com/anchor.v1.capabilities.input.InputPong";
/// Datagram payload label for the device's mirrored screen frames
/// (controller → device direction reverse: device sends ANFR/JPEG stills).
pub const MIRROR_FRAME_TYPE_URL: &str =
    "type.googleapis.com/anchor.v1.capabilities.input.RemoteScreenFrame";

pub const REMOTE_CONTROL_MIRROR_START: i32 = 1;
pub const REMOTE_CONTROL_MIRROR_STOP: i32 = 2;
pub const REMOTE_CONTROL_BACK: i32 = 3;
pub const REMOTE_CONTROL_HOME: i32 = 4;
pub const REMOTE_CONTROL_RECENTS: i32 = 5;

pub fn advertisement() -> CapabilityAdvertisement {
    CapabilityAdvertisement {
        name: CAPABILITY_NAME.into(),
        major: CAPABILITY_MAJOR,
        record_type_urls: vec![
            KEY_TYPE_URL.into(),
            TEXT_TYPE_URL.into(),
            POINTER_ABSOLUTE_TYPE_URL.into(),
            POINTER_RELATIVE_TYPE_URL.into(),
            POINTER_BUTTON_TYPE_URL.into(),
            SCROLL_TYPE_URL.into(),
            REMOTE_CONTROL_TYPE_URL.into(),
            MIRROR_FRAME_TYPE_URL.into(),
            PING_TYPE_URL.into(),
            PONG_TYPE_URL.into(),
        ],
        supports_datagrams: true,
    }
}
pub fn endpoint_advertisement() -> EndpointAdvertisement {
    EndpointAdvertisement {
        endpoint_id: ENDPOINT_ID.into(),
        capabilities: vec![advertisement()],
    }
}
/// Each record type gets a `decode_<name>` prost wrapper so decode errors
/// surface per record — one broken record must not wedge the stream.
macro_rules! decoder {
    ($name:ident, $ty:ty) => {
        pub fn $name(bytes: &[u8]) -> Result<$ty, prost::DecodeError> {
            <$ty>::decode(bytes)
        }
    };
}
use v1::capabilities::input as proto;
decoder!(decode_key, proto::InputKey);
decoder!(decode_text, proto::InputText);
decoder!(decode_pointer_absolute, proto::InputPointerAbsolute);
decoder!(decode_pointer_relative, proto::InputPointerRelative);
decoder!(decode_pointer_button, proto::InputPointerButton);
decoder!(decode_scroll, proto::InputScroll);
decoder!(decode_remote_control, proto::RemoteControl);
decoder!(decode_pong, proto::InputPong);

pub fn encode_key(hid_usage: u32, pressed: bool) -> Vec<u8> {
    proto::InputKey { hid_usage, pressed }.encode_to_vec()
}
pub fn encode_text(text_utf8: &str) -> Vec<u8> {
    proto::InputText {
        text_utf8: text_utf8.into(),
    }
    .encode_to_vec()
}
pub fn encode_pointer_relative(dx_1000ths: i32, dy_1000ths: i32) -> Vec<u8> {
    proto::InputPointerRelative {
        dx_1000ths,
        dy_1000ths,
    }
    .encode_to_vec()
}
pub fn encode_pointer_absolute(x: u32, y: u32, target_output_name: &str) -> Vec<u8> {
    proto::InputPointerAbsolute {
        x,
        y,
        target_output_name: target_output_name.into(),
    }
    .encode_to_vec()
}
pub fn encode_pointer_button(button: i32, pressed: bool) -> Vec<u8> {
    proto::InputPointerButton { button, pressed }.encode_to_vec()
}
pub fn encode_scroll(horizontal_120ths: i32, vertical_120ths: i32) -> Vec<u8> {
    proto::InputScroll {
        horizontal_120ths,
        vertical_120ths,
    }
    .encode_to_vec()
}
pub fn encode_remote_control(kind: i32) -> Vec<u8> {
    proto::RemoteControl { kind }.encode_to_vec()
}
pub fn encode_ping(sent_time_us: i64) -> Vec<u8> {
    proto::InputPing { sent_time_us }.encode_to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertisement_lists_only_defined_control_records() {
        let advert = advertisement();
        for url in [
            TEXT_TYPE_URL,
            REMOTE_CONTROL_TYPE_URL,
            MIRROR_FRAME_TYPE_URL,
        ] {
            assert!(advert.record_type_urls.iter().any(|listed| listed == url));
        }
        // The remote-control direction uses datagram flows for mirrored
        // screen frames.
        assert!(advert.supports_datagrams);
    }

    #[test]
    fn relative_motion_round_trip_preserves_signed_fixed_point() {
        let bytes = encode_pointer_relative(-1250, 875);
        let message = decode_pointer_relative(&bytes).unwrap();
        assert_eq!(message.dx_1000ths, -1250);
        assert_eq!(message.dy_1000ths, 875);
    }

    #[test]
    fn absolute_motion_round_trip_preserves_selected_output_name() {
        let expected = v1::capabilities::input::InputPointerAbsolute {
            x: 12_345,
            y: 54_321,
            target_output_name: "HEADLESS-3".into(),
        };
        let decoded = decode_pointer_absolute(&expected.encode_to_vec()).unwrap();
        assert_eq!(decoded, expected);
    }
}
