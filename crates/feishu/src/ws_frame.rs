use anyhow::Result;
use prost::Message;

#[derive(Clone, PartialEq, Message)]
pub struct FeishuHeader {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct FeishuFrame {
    #[prost(uint64, tag = "1")]
    pub seq_id: u64,
    #[prost(uint64, tag = "2")]
    pub log_id: u64,
    #[prost(int32, tag = "3")]
    pub service: i32,
    #[prost(int32, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<FeishuHeader>,
    #[prost(string, tag = "6")]
    pub payload_encoding: String,
    #[prost(string, tag = "7")]
    pub payload_type: String,
    #[prost(bytes, tag = "8")]
    pub payload: Vec<u8>,
    #[prost(string, tag = "9")]
    pub log_id_new: String,
}

pub fn decode_frame(bytes: &[u8]) -> Result<FeishuFrame> {
    FeishuFrame::decode(bytes).map_err(Into::into)
}

pub fn encode_frame(frame: &FeishuFrame) -> Vec<u8> {
    frame.encode_to_vec()
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let frame = FeishuFrame {
            seq_id: 1,
            log_id: 2,
            service: 3,
            method: 1,
            headers: vec![FeishuHeader {
                key: "type".into(),
                value: "event".into(),
            }],
            payload_encoding: String::new(),
            payload_type: String::new(),
            payload: br#"{"ok":true}"#.to_vec(),
            log_id_new: String::new(),
        };
        let bin = encode_frame(&frame);
        let decoded = decode_frame(&bin).unwrap();
        assert_eq!(decoded.seq_id, 1);
        assert_eq!(decoded.log_id, 2);
        assert_eq!(decoded.service, 3);
        assert_eq!(decoded.method, 1);
        assert_eq!(decoded.headers[0].key, "type");
        assert_eq!(decoded.headers[0].value, "event");
        assert_eq!(decoded.payload, br#"{"ok":true}"#);
    }
}
