//! Input-path regressions: the real `TextEdit` edits source indices, never display strings.
use super::{TextEdit, TextEditOutput, TextEditState};
use crate::text::{CCursor, CCursorRange};
use crate::{Context, Event, FullOutput, Id, ImeEvent, Key, Modifiers, RawInput};

fn context() -> Option<Context> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?;
    let arabic = std::path::PathBuf::from(std::env::var_os("CRAFT_FONTS_DIR")?)
        .join("fonts/noto-sans-arabic/NotoSansArabic.ttf");
    let mut fonts = crate::FontDefinitions::empty();
    for (name, path) in [("latin", root.join("assets/fonts/Inter-Regular.ttf")), ("arabic", arabic)]
    {
        fonts.font_data.insert(
            name.into(),
            std::sync::Arc::new(crate::FontData::from_owned(std::fs::read(path).ok()?)),
        );
    }
    for family in [crate::FontFamily::Proportional, crate::FontFamily::Monospace] {
        fonts.families.insert(family, vec!["latin".into(), "arabic".into()]);
    }
    let context = Context::default();
    context.set_fonts(fonts);
    Some(context)
}

fn id() -> Id {
    Id::new("bidi-input-test")
}

fn frame(ctx: &Context, text: &mut String, events: Vec<Event>) -> (TextEditOutput, FullOutput) {
    let mut editor = None;
    let raw = RawInput {
        time: Some(ctx.cumulative_frame_nr() as f64 * 0.1),
        screen_rect: Some(crate::Rect::from_min_size(crate::Pos2::ZERO, crate::vec2(640.0, 480.0))),
        events,
        ..Default::default()
    };
    let output = ctx.run_ui(raw, |ui| {
        editor = Some(
            TextEdit::multiline(text)
                .id(id())
                .font(crate::FontId::proportional(16.0))
                .desired_width(240.0)
                .show(ui),
        );
    });
    (editor.unwrap(), output)
}

fn select(ctx: &Context, start: usize, end: usize) {
    ctx.memory_mut(|m| m.request_focus(id()));
    let mut state = TextEditState::load(ctx, id()).unwrap();
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(start), CCursor::new(end))));
    state.store(ctx, id());
}

fn key(key: Key) -> Event {
    Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
}

#[test]
fn bidi_textedit_click_arrow_insert_delete_copy_and_paste_keep_logical_text() {
    let Some(ctx) = context() else { return };
    let original = "واحد 123 اثنين";
    let mut text = original.to_owned();
    frame(&ctx, &mut text, vec![]).1.drop_without_applying_deltas();
    select(&ctx, 0, 0);
    let (output, full) = frame(&ctx, &mut text, vec![key(Key::ArrowLeft)]);
    assert_eq!(output.cursor_range.unwrap().primary.index.0, 1);
    full.drop_without_applying_deltas();
    frame(&ctx, &mut text, vec![Event::Text("X".into())]).1.drop_without_applying_deltas();
    assert_eq!(text, "وXاحد 123 اثنين");
    frame(&ctx, &mut text, vec![key(Key::Backspace)]).1.drop_without_applying_deltas();
    assert_eq!(text, original);
    select(&ctx, 0, 4);
    let (output, full) = frame(&ctx, &mut text, vec![Event::Copy]);
    assert!(
        full.platform_output
            .commands
            .iter()
            .any(|c| matches!(c,crate::OutputCommand::CopyText(value) if value == "واحد"))
    );
    assert_eq!(
        output.cursor_range.unwrap().as_sorted_char_range(),
        crate::epaint::text::CharIndex(0)..crate::epaint::text::CharIndex(4)
    );
    full.drop_without_applying_deltas();
    frame(&ctx, &mut text, vec![Event::Paste("سَلَام ١٢٣".into())]).1.drop_without_applying_deltas();
    assert_eq!(text, "سَلَام ١٢٣ 123 اثنين");
}

#[test]
fn bidi_textedit_soft_separator_keeps_copy_and_caret_indices_without_a_blank_row() {
    for dpi in [1.0, 1.25, 2.0] {
        let Some(ctx) = context() else { return };
        ctx.set_pixels_per_point(dpi);
        ctx.enable_accesskit();
        let mut width = 0.0;
        ctx.run_ui(RawInput::default(), |ui| {
            width = ui.fonts_mut(|f| {
                f.layout_no_wrap(
                    "سلام".into(),
                    crate::FontId::proportional(16.0),
                    crate::Color32::WHITE,
                )
                .size()
                .x
            });
        })
        .drop_without_applying_deltas();
        let render = |text: &mut String, events| {
            let mut editor = None;
            let output = ctx.run_ui(RawInput { events, ..Default::default() }, |ui| {
                editor = Some(
                    TextEdit::multiline(text)
                        .id(id())
                        .font(crate::FontId::proportional(16.0))
                        .margin(crate::Margin::ZERO)
                        .frame(crate::Frame::NONE)
                        .desired_width(width)
                        .show(ui),
                );
            });
            (editor.unwrap(), output)
        };
        let mut text = "سلام سلام".to_owned();
        let (editor, full) = render(&mut text, vec![]);
        assert_eq!(
            editor.galley.rows.iter().map(|r| r.text()).collect::<Vec<_>>(),
            ["سلام ", "سلام"]
        );
        let tree = full.platform_output.accesskit_update.as_ref().unwrap();
        let runs: Vec<_> =
            tree.nodes.iter().filter(|(_, node)| node.role() == accesskit::Role::TextRun).collect();
        assert_eq!(
            runs.iter().map(|(_, node)| node.value().unwrap_or_default()).collect::<String>(),
            text
        );
        assert!(
            runs.iter()
                .flat_map(|(_, node)| node.character_widths().unwrap())
                .all(|width| width.is_finite() && *width >= 0.0)
        );
        full.drop_without_applying_deltas();
        select(&ctx, 4, 4);
        let (editor, full) = render(&mut text, vec![key(Key::ArrowLeft)]);
        assert_eq!(
            editor.cursor_range.unwrap().primary.index.0,
            5,
            "the soft separator advances into the next visual row"
        );
        full.drop_without_applying_deltas();
        select(&ctx, 4, 5);
        let (_, full) = render(&mut text, vec![Event::Copy]);
        assert!(full.platform_output.commands.iter().any(
            |command| matches!(command, crate::OutputCommand::CopyText(value) if value == " ")
        ));
        full.drop_without_applying_deltas();
        select(&ctx, 5, 5);
        render(&mut text, vec![key(Key::Backspace)]).1.drop_without_applying_deltas();
        assert_eq!(text, "سلامسلام");
        render(&mut text, vec![Event::Paste(" ".into())]).1.drop_without_applying_deltas();
        assert_eq!(text, "سلام سلام");
        render(&mut text, vec![Event::Text("X".into())]).1.drop_without_applying_deltas();
        assert_eq!(text, "سلام Xسلام");
    }
}

#[test]
fn bidi_textedit_mouse_click_at_mixed_boundary_inserts_at_the_clicked_word() {
    let Some(ctx) = context() else { return };
    let mut text = "abc واحد def".to_owned();
    let (output, full) = frame(&ctx, &mut text, vec![]);
    let row = &output.galley.rows[0];
    let [leading, trailing] = row.bidi.as_ref().unwrap().char_edges[7];
    let point = output.galley_pos
        + row.pos.to_vec2()
        + crate::vec2(trailing + (leading - trailing) * 0.1, row.height() * 0.5);
    full.drop_without_applying_deltas();
    frame(
        &ctx,
        &mut text,
        vec![
            Event::PointerMoved(point),
            Event::PointerButton {
                pos: point,
                button: crate::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
    )
    .1
    .drop_without_applying_deltas();
    frame(
        &ctx,
        &mut text,
        vec![Event::PointerButton {
            pos: point,
            button: crate::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
    )
    .1
    .drop_without_applying_deltas();
    frame(&ctx, &mut text, vec![Event::Text("X".into())]).1.drop_without_applying_deltas();
    assert_eq!(text, "abc واحدX def");
}

#[test]
fn bidi_accessibility_exposes_logical_text_and_rtl_character_geometry() {
    let Some(ctx) = context() else { return };
    ctx.enable_accesskit();
    let mut text = "واحد اثنين 123".to_owned();
    let (_, full) = frame(&ctx, &mut text, vec![]);
    let tree = full.platform_output.accesskit_update.as_ref().unwrap();
    let runs: Vec<_> =
        tree.nodes.iter().filter(|(_, n)| n.role() == accesskit::Role::TextRun).collect();
    assert!(!runs.is_empty());
    let logical: String = runs.iter().map(|(_, n)| n.value().unwrap_or_default()).collect();
    assert_eq!(logical, text);
    for (_, node) in runs {
        assert_eq!(node.text_direction(), Some(accesskit::TextDirection::RightToLeft));
        assert!(node.character_positions().unwrap().iter().all(|x| x.is_finite() && *x >= -1.0));
        assert!(node.character_widths().unwrap().iter().all(|x| x.is_finite() && *x >= 0.0));
        let bounds = node.bounds().unwrap();
        assert!(bounds.x0 <= bounds.x1 && bounds.y0 <= bounds.y1);
    }
    full.drop_without_applying_deltas();
}

#[test]
fn bidi_accessibility_chunks_keep_source_text_and_valid_bounds() {
    let Some(ctx) = context() else { return };
    ctx.enable_accesskit();
    let mut text = format!("{} 123", "ب".repeat(300));
    let full = ctx.run_ui(RawInput::default(), |ui| {
        TextEdit::singleline(&mut text).id(id()).show(ui);
    });
    let tree = full.platform_output.accesskit_update.as_ref().unwrap();
    let runs: Vec<_> =
        tree.nodes.iter().filter(|(_, n)| n.role() == accesskit::Role::TextRun).collect();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs.iter().map(|(_, n)| n.value().unwrap_or_default()).collect::<String>(), text);
    for (_, node) in runs {
        let positions = node.character_positions().unwrap();
        let widths = node.character_widths().unwrap();
        assert_eq!(positions.len(), widths.len());
        assert!(positions.iter().all(|x| x.is_finite() && *x >= -1.0));
        let bounds = node.bounds().unwrap();
        assert!(bounds.x0 <= bounds.x1 && bounds.y0 <= bounds.y1);
    }
    full.drop_without_applying_deltas();
}

#[test]
fn bidi_singleline_fields_align_arabic_right_and_keep_pointer_geometry_correct() {
    let Some(ctx) = context() else { return };
    let mut text = "ببب".to_owned();
    let mut editor = None;
    let full = ctx.run_ui(RawInput::default(), |ui| {
        editor = Some(TextEdit::singleline(&mut text).id(id()).desired_width(240.0).show(ui));
    });
    let output = editor.unwrap();
    assert!(
        (output.galley_pos.x + output.galley.size().x - output.text_clip_rect.right()).abs() < 1.0
    );
    let source_x = output.galley.pos_from_cursor(CCursor::new(1)).center().x;
    let point = output.galley_pos + crate::vec2(source_x, output.galley.rows[0].height() * 0.5);
    full.drop_without_applying_deltas();
    for pressed in [true, false] {
        ctx.run_ui(
            RawInput {
                events: vec![
                    Event::PointerMoved(point),
                    Event::PointerButton {
                        pos: point,
                        button: crate::PointerButton::Primary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ui| {
                TextEdit::singleline(&mut text).id(id()).desired_width(240.0).show(ui);
            },
        )
        .drop_without_applying_deltas();
    }
    let state = TextEditState::load(&ctx, id()).unwrap();
    assert_eq!(state.cursor.char_range().unwrap().primary.index.0, 1);
}

#[test]
fn bidi_textedit_ime_preedit_commit_and_candidate_position_use_logical_offsets() {
    let Some(ctx) = context() else { return };
    let mut text = "نص ".to_owned();
    frame(&ctx, &mut text, vec![]).1.drop_without_applying_deltas();
    select(&ctx, 3, 3);
    frame(&ctx, &mut text, vec![]).1.drop_without_applying_deltas();
    let (_, full) = frame(
        &ctx,
        &mut text,
        vec![Event::Ime(ImeEvent::Preedit {
            text: "سلا".into(), active_range_chars: Some(0..3)
        })],
    );
    assert_eq!(text, "نص سلا");
    let ime =
        full.platform_output.ime.as_ref().expect("focused TextEdit reports candidate geometry");
    assert!(ime.cursor_rect.is_finite());
    full.drop_without_applying_deltas();
    frame(&ctx, &mut text, vec![Event::Ime(ImeEvent::Commit("سلام 123".into()))])
        .1
        .drop_without_applying_deltas();
    assert_eq!(text, "نص سلام 123");
    let state = TextEditState::load(&ctx, id()).unwrap();
    assert_eq!(state.cursor.char_range().unwrap().primary.index.0, text.chars().count());
}
