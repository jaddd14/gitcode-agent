use super::Parser;
use crate::event::{Event, InternalEvent, KeyCode};

#[test]
fn device_attributes_are_consumed_without_events() {
    for reply in [b"\x1b[>0;10;1c".as_slice(), b"\x1b[=0;10;1c", b"\x1b[>c"] {
        let mut parser = Parser::default();
        parser.advance(reply, false);
        assert_eq!(parser.next(), None);
        assert!(parser.buffer.is_empty());
        parser.advance(b"x", false);
        assert_eq!(
            parser.next(),
            Some(InternalEvent::Event(Event::Key(KeyCode::Char('x').into())))
        );
        assert_eq!(parser.next(), None);
        assert!(parser.buffer.is_empty());
    }
}

#[test]
fn fragmented_device_attributes_wait_for_final_byte() {
    for reply in [b"\x1b[>0;10;1c".as_slice(), b"\x1b[=0;10;1c"] {
        let mut parser = Parser::default();
        for (index, byte) in reply.iter().enumerate() {
            parser.advance(&[*byte], index == 0);
            assert_eq!(parser.next(), None);
            if index + 1 < reply.len() {
                assert_eq!(parser.buffer, reply[..=index]);
            } else {
                assert!(parser.buffer.is_empty());
            }
        }
        parser.advance(b"ok", false);
        for ch in ['o', 'k'] {
            assert_eq!(
                parser.next(),
                Some(InternalEvent::Event(Event::Key(KeyCode::Char(ch).into())))
            );
        }
        assert_eq!(parser.next(), None);
        assert!(parser.buffer.is_empty());
    }
}

#[test]
fn device_attributes_preserve_following_input_at_every_split() {
    let input = b"\x1b[>0;10;1c\x1b[=0;10;1cx\x1b[A\x1b[97u";
    for split in 0..=input.len() {
        let mut parser = Parser::default();
        parser.advance(&input[..split], true);
        parser.advance(&input[split..], false);
        assert_eq!(
            parser.collect::<Vec<_>>(),
            vec![
                InternalEvent::Event(Event::Key(KeyCode::Char('x').into())),
                InternalEvent::Event(Event::Key(KeyCode::Up.into())),
                InternalEvent::Event(Event::Key(KeyCode::Char('a').into())),
            ]
        );
    }
}

#[cfg(feature = "bracketed-paste")]
#[test]
fn device_attributes_do_not_consume_paste_contents() {
    let mut parser = Parser::default();
    parser.advance(b"\x1b[>0;10;1c\x1b[200~\x1b[>0;10;1c\x1b[201~z", false);
    assert_eq!(
        parser.next(),
        Some(InternalEvent::Event(Event::Paste("\x1b[>0;10;1c".into())))
    );
    assert_eq!(
        parser.next(),
        Some(InternalEvent::Event(Event::Key(KeyCode::Char('z').into())))
    );
    assert_eq!(parser.next(), None);
    assert!(parser.buffer.is_empty());
}

#[test]
fn device_attributes_do_not_replace_keyboard_negotiation() {
    let mut parser = Parser::default();
    parser.advance(b"\x1b[>0;10;1c\x1b[?1u\x1b[?1;2c", false);
    assert_eq!(
        parser.next(),
        Some(InternalEvent::KeyboardEnhancementFlags(
            crate::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        ))
    );
    assert_eq!(parser.next(), Some(InternalEvent::PrimaryDeviceAttributes));
    assert_eq!(parser.next(), None);
    assert!(parser.buffer.is_empty());
}
