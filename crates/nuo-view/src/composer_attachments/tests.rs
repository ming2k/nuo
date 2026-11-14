use super::*;

#[test]
fn should_chip_paste_thresholds() {
    // Short and even multi-line: inline.
    assert!(!should_chip_paste("hello"));
    assert!(!should_chip_paste(&"a".repeat(1024)));
    assert!(!should_chip_paste("a\nb\nc\nd"));
    // At least one char past the threshold: chip.
    assert!(should_chip_paste(&"a".repeat(1025)));
    // A many-line paste under the char threshold still inlines.
    let many_short_lines = "x\n".repeat(100);
    assert!(!should_chip_paste(many_short_lines.trim_end()));
}

#[test]
fn paste_line_count_matches_str_lines() {
    assert_eq!(paste_line_count(""), 0);
    assert_eq!(paste_line_count("one"), 1);
    assert_eq!(paste_line_count("a\nb"), 2);
    assert_eq!(paste_line_count("a\nb\n"), 2);
}

#[test]
fn human_size_formatting() {
    assert_eq!(human_size(0), "0 B");
    assert_eq!(human_size(900), "900 B");
    assert_eq!(human_size(1023), "1023 B");
    assert_eq!(human_size(1024), "1.0 KB");
    assert_eq!(human_size(1536), "1.5 KB");
    assert_eq!(human_size(120 * 1024), "120 KB");
    assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
}

#[test]
fn base64_byte_size_recovers_raw_length() {
    // Standard base64 of 1, 2, 3 and 4 raw bytes (padding included).
    assert_eq!(base64_byte_size("YQ=="), 1); // "a"
    assert_eq!(base64_byte_size("YWI="), 2); // "ab"
    assert_eq!(base64_byte_size("YWJj"), 3); // "abc"
    assert_eq!(base64_byte_size("YWJjZA=="), 4); // "abcd"
    assert_eq!(base64_byte_size(""), 0);
}

#[test]
fn image_chip_format() {
    assert_eq!(image_chip(1, 0), "[Image #1 (0 B)]");
    assert_eq!(image_chip(7, 1536), "[Image #7 (1.5 KB)]");
}

#[test]
fn paste_chip_format() {
    assert_eq!(paste_chip(1, 5, 0), "[Pasted text #1 +5 lines (0 B)]");
    assert_eq!(paste_chip(3, 0, 1024), "[Pasted text #3 +0 lines (1.0 KB)]");
}

#[test]
fn iter_chips_finds_image_and_paste() {
    let input = format!(
        "pre {} mid {} end",
        image_chip(1, 2048),
        paste_chip(2, 10, 5120)
    );
    let chips = iter_chips(&input);
    assert_eq!(chips.len(), 2);
    assert_eq!(chips[0].kind, ChipKind::Image);
    assert_eq!(chips[0].number, 1);
    assert_eq!(chips[0].line_count, None);
    assert_eq!(chips[1].kind, ChipKind::Paste);
    assert_eq!(chips[1].number, 2);
    assert_eq!(chips[1].line_count, Some(10));
    // Byte ranges point at the actual chip substrings.
    assert_eq!(
        &input[chips[0].start_byte..chips[0].end_byte],
        "[Image #1 (2.0 KB)]"
    );
    assert_eq!(
        &input[chips[1].start_byte..chips[1].end_byte],
        "[Pasted text #2 +10 lines (5.0 KB)]"
    );
}

#[test]
fn parse_chip_body_accepts_legacy_and_size_badged_forms() {
    // Legacy bare forms still parse (input edited by hand, old drafts).
    assert_eq!(
        parse_chip_body("Image #1"),
        Some((ChipKind::Image, 1, None))
    );
    assert_eq!(
        parse_chip_body("Pasted text #1 +5 lines"),
        Some((ChipKind::Paste, 1, Some(5)))
    );
    // Current and legacy size-badged forms parse; the badge is not trusted.
    assert_eq!(
        parse_chip_body("Image #3 (24.1 KB)"),
        Some((ChipKind::Image, 3, None))
    );
    assert_eq!(
        parse_chip_body("Image #3 | 24.1 KB"),
        Some((ChipKind::Image, 3, None))
    );
    assert_eq!(
        parse_chip_body("Pasted text #2 +12 lines (5.0 KB)"),
        Some((ChipKind::Paste, 2, Some(12)))
    );
    assert_eq!(
        parse_chip_body("Pasted text #2 +12 lines | 5.0 KB"),
        Some((ChipKind::Paste, 2, Some(12)))
    );
    // Zero and non-numeric numbers are rejected.
    assert_eq!(parse_chip_body("Image #0 | 1 KB"), None);
    assert_eq!(parse_chip_body("Pasted text #x"), None);
}

#[test]
fn iter_chips_ignores_unrelated_brackets() {
    let chips = iter_chips("hello [not a chip] world [1] [Image]");
    assert!(chips.is_empty());
}

#[test]
fn chip_range_ending_at_detects_trailing_chip() {
    let input = format!("hello {}", image_chip(1, 42));
    let cursor = input.len();
    assert_eq!(chip_range_ending_at(&input, cursor), Some((6, cursor)));
    // Cursor one byte inside the chip: no match.
    assert_eq!(chip_range_ending_at(&input, cursor - 1), None);
    // Cursor at the space after "hello": no match.
    assert_eq!(chip_range_ending_at(&input, 5), None);
}

#[test]
fn chip_range_for_backspace_handles_trailing_space() {
    let input = format!("hello {} ", image_chip(1, 42));
    let cursor = input.len();
    // Cursor sits after the space; backspace should remove both the
    // space and the chip in one keystroke.
    let chip_start = "hello ".len();
    assert_eq!(
        chip_range_for_backspace(&input, cursor),
        Some((chip_start, cursor))
    );
}

#[test]
fn chip_range_for_backspace_handles_no_trailing_space() {
    let input = format!("hello{}", image_chip(1, 42));
    let cursor = input.len();
    let chip_start = "hello".len();
    assert_eq!(
        chip_range_for_backspace(&input, cursor),
        Some((chip_start, cursor))
    );
}

#[test]
fn chip_range_for_delete_eats_leading_chip_and_trailing_space() {
    let input = format!("hello {} world", image_chip(1, 42));
    let cursor = "hello ".len();
    // Forward Delete at the chip start must remove the chip *and* the
    // one trailing space a paste inserts, so the surviving text reads
    // "hello world" not "hello  world".
    let chip_end = input.find(" world").expect("world follows chip") + 1;
    assert_eq!(
        chip_range_for_delete(&input, cursor),
        Some((cursor, chip_end))
    );
    assert_eq!(
        format!("{}world", &input[..cursor]),
        "hello world",
        "sanity: deleting the range must splice cleanly"
    );
}

#[test]
fn chip_range_for_delete_handles_end_of_input() {
    // Chip at the very end with no trailing space: Delete at its start
    // removes just the chip.
    let input = format!("hello {}", image_chip(1, 42));
    let cursor = "hello ".len();
    assert_eq!(
        chip_range_for_delete(&input, cursor),
        Some((cursor, input.len()))
    );
}

#[test]
fn chip_range_for_delete_rejects_non_chips() {
    // A plain bracketed word is not a chip; Delete must fall through to
    // the ordinary single-character delete.
    let input = "see [not a chip] here".to_string();
    let cursor = "see ".len();
    assert_eq!(chip_range_for_delete(&input, cursor), None);
    // Neither is a chip whose body fails to parse.
    assert_eq!(chip_range_for_delete(&input, 0), None);
}

#[test]
fn reconcile_drops_orphaned_pending_entries() {
    // Input has one image chip but pending_images holds two. Reconcile
    // must truncate the second entry and relabel the surviving chip.
    let mut images = vec![
        ImagePart {
            mime: "image/png".to_string(),
            data: "a".to_string(),
        },
        ImagePart {
            mime: "image/png".to_string(),
            data: "b".to_string(),
        },
    ];
    let mut pastes = Vec::<String>::new();
    let input = format!("look at {}", image_chip(1, base64_byte_size("a")));
    let out = reconcile(&input, &mut images, &mut pastes);
    assert_eq!(out, input);
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].data, "a");
}

#[test]
fn reconcile_relabels_chips_after_a_middle_deletion() {
    // User pasted three images, then deleted the chip for #2 by hand
    // (e.g. via selection delete). The remaining two chips must be
    // relabeled to #1 and #2 so their numbers stay contiguous with
    // the surviving pending_images entries.
    let mut images = vec![
        ImagePart {
            mime: "image/png".to_string(),
            data: "a".to_string(),
        },
        ImagePart {
            mime: "image/png".to_string(),
            data: "b".to_string(),
        },
        ImagePart {
            mime: "image/png".to_string(),
            data: "c".to_string(),
        },
    ];
    let mut pastes = Vec::<String>::new();
    // Original: "x [Image #1] y [Image #2] z [Image #3]" — but #2 was
    // removed, leaving #1 and #3 out of order.
    let input = format!(
        "x {} y {} z",
        image_chip(1, base64_byte_size("a")),
        image_chip(3, base64_byte_size("c")),
    );
    let out = reconcile(&input, &mut images, &mut pastes);
    assert_eq!(
        out,
        format!(
            "x {} y {} z",
            image_chip(1, base64_byte_size("a")),
            image_chip(2, base64_byte_size("c")),
        )
    );
    assert_eq!(images.len(), 2);
    // The first surviving chip pulls pending_images[0] = "a", the
    // second pulls pending_images[2] = "c".
    assert_eq!(images[0].data, "a");
    assert_eq!(images[1].data, "c");
}

#[test]
fn reconcile_resizes_badges_from_surviving_payloads() {
    // A relabeled chip must re-report the payload it actually carries,
    // not whatever badge the hand-edited input happened to show. Here
    // both chips claim 9999 bytes but the surviving payloads are tiny,
    // so reconcile rewrites the badges to the real counts.
    let mut images = vec![ImagePart {
        mime: "image/png".to_string(),
        data: "YQ==".to_string(), // base64 of the single byte "a"
    }];
    let mut pastes = vec!["hello".to_string()];
    let input = format!("{} {}", image_chip(1, 9999), paste_chip(1, 2, 9999));
    let out = reconcile(&input, &mut images, &mut pastes);
    assert_eq!(
        out,
        format!(
            "{} {}",
            image_chip(1, base64_byte_size(&images[0].data)),
            paste_chip(1, 2, pastes[0].len()),
        )
    );
}

#[test]
fn reconcile_preserves_paste_line_counts() {
    let mut images = Vec::<ImagePart>::new();
    let mut pastes = vec!["first\npaste".to_string(), "second\npaste".to_string()];
    let input = format!(
        "{} {}",
        paste_chip(1, 2, pastes[0].len()),
        paste_chip(2, 2, pastes[1].len()),
    );
    let out = reconcile(&input, &mut images, &mut pastes);
    assert_eq!(out, input);
    assert_eq!(pastes.len(), 2);
    let _ = out;
}

#[test]
fn reconcile_clears_pending_lists_when_input_has_no_chips() {
    let mut images = vec![ImagePart {
        mime: "image/png".to_string(),
        data: "a".to_string(),
    }];
    let mut pastes = vec!["stale".to_string()];
    let out = reconcile("just plain text", &mut images, &mut pastes);
    assert_eq!(out, "just plain text");
    assert!(images.is_empty());
    assert!(pastes.is_empty());
}

#[test]
fn expand_paste_chips_inlines_text_in_order() {
    let pastes = vec!["AAA".to_string(), "BBB".to_string()];
    let input = format!(
        "pre {} mid {} post",
        paste_chip(1, 1, pastes[0].len()),
        paste_chip(2, 1, pastes[1].len()),
    );
    let out = expand_paste_chips(&input, &pastes);
    assert_eq!(out, "pre AAA mid BBB post");
}

#[test]
fn expand_paste_chips_keeps_image_chips() {
    // Image chips are positional labels for the model, not paste
    // payloads — they must survive expansion unchanged.
    let pastes = vec!["AAA".to_string()];
    let input = format!(
        "{} {} {}",
        image_chip(1, 2048),
        paste_chip(1, 1, pastes[0].len()),
        image_chip(2, 1024),
    );
    let out = expand_paste_chips(&input, &pastes);
    assert_eq!(
        out,
        format!("{} AAA {}", image_chip(1, 2048), image_chip(2, 1024))
    );
}

#[test]
fn expand_paste_chips_passthrough_when_no_chips() {
    let pastes: Vec<String> = Vec::new();
    let out = expand_paste_chips("plain text only", &pastes);
    assert_eq!(out, "plain text only");
}

#[test]
fn expand_paste_chips_drops_orphaned_paste_chips() {
    // A paste chip with no staged payload (e.g. recalled from a history
    // entry recorded before attachment staging) must not ship to the
    // model as a literal placeholder. The chip and its surrounding paste
    // spaces are collapsed so the remaining words stay single-spaced.
    let pastes: Vec<String> = Vec::new();
    let input = format!(
        "pre {} mid {}",
        paste_chip(1, 5, 5120),
        paste_chip(2, 3, 2048)
    );
    let out = expand_paste_chips(&input, &pastes);
    assert_eq!(out, "pre mid");

    // A backed chip inlines while a later orphan drops.
    let pastes = vec!["AAA".to_string()];
    let input = format!("x {} y {}", paste_chip(1, 1, 3), paste_chip(2, 1, 9999));
    assert_eq!(expand_paste_chips(&input, &pastes), "x AAA y");
}

#[test]
fn strip_orphan_image_chips_keeps_backed_chips() {
    // With the payload staged (the normal submit path), every chip whose
    // `#N` is within `images` survives as a positional label.
    let input = format!(
        "look at {} and {}",
        image_chip(1, 2048),
        image_chip(2, 1024)
    );
    let out = strip_orphan_image_chips(&input, 2);
    assert_eq!(out, input);
}

#[test]
fn strip_orphan_image_chips_drops_unbacked_chips() {
    // No payload staged (recalled from history recorded before
    // attachment staging, or hand-typed): every image chip is orphaned
    // and dropped, together with one following space.
    let input = format!("look at {} here", image_chip(1, 2048));
    assert_eq!(strip_orphan_image_chips(&input, 0), "look at here");

    // A mid-list orphan drops while its backed siblings survive.
    let input = format!("x {} y {} z", image_chip(1, 2048), image_chip(2, 1024));
    assert_eq!(
        strip_orphan_image_chips(&input, 1),
        format!("x {} y z", image_chip(1, 2048))
    );
}

#[test]
fn strip_orphan_image_chips_passthrough_without_chips() {
    assert_eq!(
        strip_orphan_image_chips("plain text only", 0),
        "plain text only"
    );
}
