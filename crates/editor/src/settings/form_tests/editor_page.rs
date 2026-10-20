//! The Editor page: the generic schema form with steppers.

use super::*;

// ---- The Editor page is the generic schema form, with steppers ----

const EDITOR: &str = crate::settings::editor_prefs::EDITOR_PATH;

fn row_at(rows: &[Row], at: &str) -> Row {
    rows.iter()
        .find(|r| r.path == p(at))
        .cloned()
        .unwrap_or_else(|| panic!("{at}: no row"))
}

fn step_of(rows: &[Row], at: &str) -> form::Step {
    match row_at(rows, at).kind {
        Kind::Number { step: Some(s), .. } => s,
        k => panic!("{at}: {k:?}"),
    }
}

/// GUI reachability (SR12): every `editor_schema()` field — nested records included — has a
/// form row on the Editor page, so `scroll_anchor` and `open_url_cmd` are editable.
#[test]
fn every_editor_schema_field_yields_a_form_row() {
    fn walk(r: &cv_schema::Record, at: &Path, out: &mut Vec<Path>) {
        for f in r.fields {
            let fp = at.key(f.key);
            if let Node::Record(sub) = f.node {
                walk(sub, &fp, out);
            }
            out.push(fp);
        }
    }
    let Node::Record(r) = cv_schema::editor_schema() else {
        panic!("record")
    };
    let mut want = Vec::new();
    walk(r, &Path::root(), &mut want);
    assert!(want.len() >= 15, "{}", want.len());
    let st = loaded(EDITOR, "");
    let rows = rows_of(&st, EDITOR);
    for w in want {
        assert!(rows.iter().any(|r| r.path == w), "{w}: no row");
    }
    assert!(matches!(
        row_at(&rows, "scroll_anchor").kind,
        Kind::Number { step: Some(_), .. }
    ));
    assert!(matches!(
        row_at(&rows, "open_url_cmd").kind,
        Kind::Chips { .. }
    ));
}

/// Decision 3: bounded numbers step from the stored value (else the default) by the schema's
/// step, as the old Editor page did (font ±1, UI scale ±0.25, limit ±10, debounce ±50).
#[test]
fn bounded_numbers_get_steppers_with_the_old_increments() {
    let st = loaded(EDITOR, "ui_scale = 2.9\n[search]\nlimit = 995\n");
    let rows = rows_of(&st, EDITOR);
    let f = step_of(&rows, "font_size");
    assert_eq!(
        (f.dec, f.inc),
        (
            Some(ScalarValue::Float(21.0)),
            Some(ScalarValue::Float(23.0))
        ),
        "absent: from the default 22"
    );
    let u = step_of(&rows, "ui_scale");
    assert_eq!(
        (u.dec, u.inc),
        (Some(ScalarValue::Float(2.65)), None),
        "3.15 is past the bound"
    );
    let l = step_of(&rows, "search.limit");
    assert_eq!(
        (l.dec, l.inc),
        (Some(ScalarValue::Int(985)), Some(ScalarValue::Int(1000))),
        "integers clamp"
    );
    let d = step_of(&rows, "search.debounce_ms");
    assert_eq!(
        (d.dec, d.inc),
        (Some(ScalarValue::Int(200)), Some(ScalarValue::Int(300)))
    );
    let a = step_of(&rows, "scroll_anchor");
    assert_eq!((a.dec, a.inc), (None, Some(ScalarValue::Int(5))));
    // Every page gains it: a bounded field elsewhere steps too; an unbounded one does not.
    let st = loaded(
        PIPES,
        "[[daily_brief.weather]]\nlabel = \"Home\"\nlat = 1.0\nlon = 2.0\n",
    );
    let mut st = st;
    st.form_msg(PIPES, FormMsg::Toggle(p("daily_brief.weather[0]")));
    let rows = rows_of(&st, PIPES);
    assert!(step_of(&rows, "daily_brief.weather[0].lat").inc.is_some());
    assert!(matches!(
        row_at(&rows, "image_describe.max_tokens").kind,
        Kind::Number { step: Some(_), .. }
    ));
    // An unbounded number (a view's `limit`) gets no stepper.
    let mut st = Settings::default();
    let mut f = ConfigFile::new(
        "_custom/crucible-vault/views/v.toml",
        Some(cv_schema::view_schema()),
    );
    f.apply_read(Ok((
        b"[[views]]\ntype = \"table\"\nname = \"T\"\nlimit = 3\n".to_vec(),
        ContentHash::from_hex("00"),
    )));
    st.files.insert(f.path().to_owned(), f);
    st.form_msg(
        "_custom/crucible-vault/views/v.toml",
        FormMsg::Toggle(p("views[0]")),
    );
    let rows = rows_of(&st, "_custom/crucible-vault/views/v.toml");
    assert!(matches!(
        row_at(&rows, "views[0].limit").kind,
        Kind::Number { step: None, .. }
    ));
}

/// The on-disk contract: generic-form edits of every `editor.toml` field write the bytes the
/// hand page wrote (same keys, value types, list shape and enum spellings; comments kept).
#[test]
fn editor_page_edits_write_the_hand_page_bytes() {
    let mut st = loaded(EDITOR, "# my prefs\nfont_size = 22 # base\n");
    let step = |st: &Settings, at: &str, inc: bool| {
        let s = step_of(&rows_of(st, EDITOR), at);
        if inc { s.inc } else { s.dec }.expect("enabled")
    };
    let v = step(&st, "font_size", true);
    st.form_msg(
        EDITOR,
        FormMsg::Step {
            path: p("font_size"),
            value: v,
        },
    );
    let v = step(&st, "ui_scale", true);
    st.form_msg(
        EDITOR,
        FormMsg::Step {
            path: p("ui_scale"),
            value: v,
        },
    );
    let v = step(&st, "search.limit", false);
    st.form_msg(
        EDITOR,
        FormMsg::Step {
            path: p("search.limit"),
            value: v,
        },
    );
    let v = step(&st, "search.debounce_ms", true);
    st.form_msg(
        EDITOR,
        FormMsg::Step {
            path: p("search.debounce_ms"),
            value: v,
        },
    );
    for (k, v) in [
        ("surround", "dark"),
        ("tab_title", "path"),
        ("quick_switch_open", "switch"),
        ("delete_mode", "permanent"),
        ("missing_link", "create"),
    ] {
        // The seg-group writes the choice string (what `Kind::Enum` offers).
        assert!(matches!(
            row_at(&rows_of(&st, EDITOR), k).kind,
            Kind::Enum { choices, .. } if choices.contains(&v)
        ));
        st.edit(EDITOR, &DocEdit::Set(p(k), ScalarValue::Str(v.into())))
            .expect("enum");
    }
    st.edit(
        EDITOR,
        &DocEdit::Set(p("focus_follows_mouse"), ScalarValue::Bool(true)),
    )
    .expect("toggle");
    for id in ["a", "b"] {
        st.form_msg(
            EDITOR,
            FormMsg::ChipAdd {
                path: p("palette.hidden"),
                value: Some(id.into()),
            },
        );
    }
    assert_eq!(
        doc_text(&st, EDITOR),
        "# my prefs\nfont_size = 23.0 # base\nui_scale = 1.25\nsurround = \"dark\"\ntab_title = \"path\"\nquick_switch_open = \"switch\"\ndelete_mode = \"permanent\"\nmissing_link = \"create\"\nfocus_follows_mouse = true\n\n[search]\nlimit = 40\ndebounce_ms = 300\n\n[palette]\nhidden = [\"a\", \"b\"]\n"
    );
    // The last chip removed leaves `hidden = []`, as the emptied text field did.
    for _ in 0..2 {
        st.form_msg(
            EDITOR,
            FormMsg::ChipRemove {
                path: p("palette.hidden"),
                index: 0,
            },
        );
    }
    assert!(doc_text(&st, EDITOR).contains("hidden = []"));
}

/// A stepper press through the app applies the pref live and drops a stale typed draft.
#[test]
fn editor_stepper_applies_live_and_clears_the_draft() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d);
    let _ = app.send(s(SettingsMsg::Form(
        EDITOR.into(),
        FormMsg::Number {
            path: p("font_size"),
            text: "3x".into(),
            float: true,
        },
    )));
    let _ = app.send(s(SettingsMsg::Form(
        EDITOR.into(),
        FormMsg::Step {
            path: p("font_size"),
            value: ScalarValue::Float(30.0),
        },
    )));
    assert!((app.prefs.font_size - 30.0).abs() < f32::EPSILON);
    let rows = rows_of(&app.settings, EDITOR);
    assert!(matches!(
        row_at(&rows, "font_size").kind,
        Kind::Number { text, .. } if text == "30.0"
    ));
    // The surround seg mirrors into the local UiState copy.
    let _ = app.send(s(SettingsMsg::Edit(
        EDITOR.into(),
        DocEdit::Set(p("surround"), ScalarValue::Str("light".into())),
    )));
    assert_eq!(app.prefs.surround, crate::theme::Surround::Light);
    assert_eq!(app.ui.surround, crate::theme::Surround::Light);
}

/// SM11: one table row per section, every file under the one config dir and named in
/// `cv_schema::FILES`.
#[test]
fn section_table_rows_match_their_sections_and_files() {
    for (i, sec) in Section::ALL.into_iter().enumerate() {
        assert_eq!(SECTIONS[i].section, sec);
        if let Some(f) = sec.file() {
            assert!(f.starts_with(CONFIG_DIR), "{f}");
            let name = sec.file_name().expect("name");
            assert!(cv_schema::FILES.contains(&name), "{name}");
            assert_eq!(Section::for_file_name(name), Some(sec));
        }
    }
    assert_eq!(crate::palette::CONFIG_PREFIX, CONFIG_DIR);
    assert!(VIEWS_DIR.starts_with(CONFIG_DIR));
}
