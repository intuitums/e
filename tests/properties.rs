//! Cross-cutting properties for parsers and render sanitizers. Fixed examples
//! live with their subsystem tests; these vary boundaries and arbitrary text.

use e::core::api::{parse_incoming, Incoming};
use e::core::providers::SseSplitter;
use e::core::tools::sanitize_display;
use e::tui::markdown::{visible_width, wrap_styled};
use proptest::prelude::*;

proptest! {
    #[test]
    fn sse_result_does_not_depend_on_transport_chunking(cuts in prop::collection::vec(0usize..256, 0..32)) {
        let input = b"event: message\r\ndata: {\"text\":\"h\xc3\xa9llo\"}\r\n\r\ndata: second\ndata: line\n\n";
        let mut whole = SseSplitter::new();
        let expected = whole.feed_bytes(input);

        let mut split = SseSplitter::new();
        let mut actual = Vec::new();
        let mut at = 0;
        for size in cuts {
            if at == input.len() {
                break;
            }
            let end = (at + size.max(1)).min(input.len());
            actual.extend(split.feed_bytes(&input[at..end]));
            at = end;
        }
        actual.extend(split.feed_bytes(&input[at..]));
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn sanitized_output_contains_only_displayable_controls(input in any::<String>()) {
        let clean = sanitize_display(&input);
        let only_displayable = clean.chars().all(|character| {
            !character.is_control() || character == '\n'
        });
        prop_assert!(only_displayable);
    }

    #[test]
    fn plain_text_wrapping_respects_display_width(input in "[^\\x1b\\r\\n]*", width in 2usize..80) {
        for row in wrap_styled(&input, width) {
            prop_assert!(visible_width(&row) <= width,
                "row width {} exceeded {width}: {row:?}", visible_width(&row));
        }
    }

    #[test]
    fn extension_error_round_trips_arbitrary_text(id in any::<u64>(), message in any::<String>()) {
        let line = serde_json::json!({"id": id, "error": message}).to_string();
        match parse_incoming(&line) {
            Some(Incoming::Response { id: parsed, result: Err(error) }) => {
                prop_assert_eq!(parsed, id);
                prop_assert_eq!(error, message);
            }
            _ => prop_assert!(false, "valid response was not parsed"),
        }
    }
}
