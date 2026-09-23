use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

/// Frame layout (frozen): u32_be len, u8 kind, payload[len - 1].
/// `len` covers the kind byte plus the payload.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

pub const KIND_CTL: u8 = 0;
pub const KIND_OUT: u8 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct RawFrame {
  pub kind: u8,
  pub payload: Bytes,
}

/// Stateless: a frame is taken from the buffer only once it is complete,
/// so undecoded bytes always start at a frame boundary and can be handed
/// to another decoder.
pub struct FrameCodec;

fn protocol_error(msg: String) -> std::io::Error {
  std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

impl Decoder for FrameCodec {
  type Item = RawFrame;
  type Error = std::io::Error;

  fn decode(
    &mut self,
    src: &mut BytesMut,
  ) -> Result<Option<Self::Item>, Self::Error> {
    let Some(header) = src.first_chunk::<4>() else {
      return Ok(None);
    };
    let len = u32::from_be_bytes(*header) as usize;
    if len < 1 || len > MAX_FRAME {
      return Err(protocol_error(format!("bad frame length: {len}")));
    }
    if src.len() < 4 + len {
      src.reserve(4 + len - src.len());
      return Ok(None);
    }

    src.advance(4);
    let mut frame = src.split_to(len);
    let kind = frame.get_u8();
    Ok(Some(RawFrame {
      kind,
      payload: frame.freeze(),
    }))
  }
}

impl Encoder<RawFrame> for FrameCodec {
  type Error = std::io::Error;

  fn encode(
    &mut self,
    frame: RawFrame,
    dst: &mut BytesMut,
  ) -> Result<(), Self::Error> {
    let len = frame.payload.len() + 1;
    if len > MAX_FRAME {
      return Err(protocol_error(format!("frame too large: {len}")));
    }
    dst.reserve(4 + len);
    dst.put_u32(len as u32);
    dst.put_u8(frame.kind);
    dst.extend_from_slice(&frame.payload);
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn encode(frame: RawFrame) -> BytesMut {
    let mut buf = BytesMut::new();
    FrameCodec.encode(frame, &mut buf).unwrap();
    buf
  }

  #[test]
  fn round_trip() {
    let frame = RawFrame {
      kind: KIND_CTL,
      payload: Bytes::from_static(b"{\"type\":\"hello\"}"),
    };
    let mut buf = encode(frame.clone());
    let decoded = FrameCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(decoded, frame);
    assert!(buf.is_empty());
  }

  #[test]
  fn empty_payload_round_trips() {
    let frame = RawFrame {
      kind: KIND_OUT,
      payload: Bytes::new(),
    };
    let mut buf = encode(frame.clone());
    let decoded = FrameCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(decoded, frame);
  }

  #[test]
  fn partial_frame_waits_for_more_data() {
    let buf = encode(RawFrame {
      kind: KIND_OUT,
      payload: Bytes::from_static(b"hello world"),
    });
    let mut codec = FrameCodec;
    let mut partial = BytesMut::from(&buf[..7]);
    assert_eq!(codec.decode(&mut partial).unwrap(), None);
    // Nothing is consumed until the frame is complete.
    assert_eq!(&partial[..], &buf[..7]);
    partial.extend_from_slice(&buf[7..]);
    let decoded = codec.decode(&mut partial).unwrap().unwrap();
    assert_eq!(decoded.payload, Bytes::from_static(b"hello world"));
  }

  #[test]
  fn split_header_waits_for_more_data() {
    let buf = encode(RawFrame {
      kind: KIND_CTL,
      payload: Bytes::from_static(b"x"),
    });
    let mut codec = FrameCodec;
    let mut partial = BytesMut::from(&buf[..2]);
    assert_eq!(codec.decode(&mut partial).unwrap(), None);
    partial.extend_from_slice(&buf[2..]);
    assert!(codec.decode(&mut partial).unwrap().is_some());
  }

  #[test]
  fn back_to_back_frames_decode_separately() {
    let mut buf = encode(RawFrame {
      kind: KIND_CTL,
      payload: Bytes::from_static(b"a"),
    });
    buf.extend_from_slice(&encode(RawFrame {
      kind: KIND_OUT,
      payload: Bytes::from_static(b"b"),
    }));
    let mut codec = FrameCodec;
    assert_eq!(codec.decode(&mut buf).unwrap().unwrap().kind, KIND_CTL);
    assert_eq!(codec.decode(&mut buf).unwrap().unwrap().kind, KIND_OUT);
    assert_eq!(codec.decode(&mut buf).unwrap(), None);
  }

  #[test]
  fn zero_length_is_fatal() {
    let mut buf = BytesMut::new();
    buf.put_u32(0);
    assert!(FrameCodec.decode(&mut buf).is_err());
  }

  #[test]
  fn oversize_length_is_fatal() {
    let mut buf = BytesMut::new();
    buf.put_u32((MAX_FRAME + 1) as u32);
    assert!(FrameCodec.decode(&mut buf).is_err());
  }

  #[test]
  fn oversize_payload_fails_to_encode() {
    let frame = RawFrame {
      kind: KIND_OUT,
      payload: Bytes::from(vec![0u8; MAX_FRAME]),
    };
    let mut buf = BytesMut::new();
    assert!(FrameCodec.encode(frame, &mut buf).is_err());
  }

  #[test]
  fn unknown_kind_is_returned_for_caller_to_skip() {
    let mut buf = encode(RawFrame {
      kind: 0x7f,
      payload: Bytes::from_static(b"future"),
    });
    let decoded = FrameCodec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(decoded.kind, 0x7f);
  }
}
