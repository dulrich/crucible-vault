use super::*;

/// Plan round-8 WP-3: `checkbox = true` reaches the result `Column` on `note.*` ids only;
/// the default is false.
#[test]
fn checkbox_property_reaches_the_column() {
    let def = "[properties.read]\ncheckbox = true\n\n[properties.\"file.name\"]\ncheckbox = true\n\n[[views]]\ntype = \"table\"\nname = \"T\"\norder = [\"file.name\", \"note.read\", \"word-count\"]\n";
    let fx = fixture(&[("cb.toml", def)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let cols = fx.run(&mut h, "cb#T", None).columns;
    let flags: Vec<bool> = cols
        .iter()
        .map(|c| matches!(c.role, Some(crate::result::ColumnRole::Checkbox { .. })))
        .collect();
    assert_eq!(flags, [false, true, false]);
    assert_eq!(
        cols[1].role,
        Some(crate::result::ColumnRole::Checkbox { key: "read".into() })
    );
}

/// Plan intake-round-2 WP-4: `columnSize` travels def → compiled view → result JSON → decode;
/// non-positive sizes are ignored and an old payload without `width` still decodes.
#[test]
fn column_size_reaches_the_result() {
    let def = "[[views]]\ntype = \"table\"\nname = \"Sized\"\norder = [\"file.name\", \"word-count\", \"note.author\"]\ncolumnSize = { \"note.word-count\" = 90, \"author\" = 0 }\n";
    let fx = fixture(&[("cs.toml", def)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let r = fx.run(&mut h, "cs#Sized", None);
    let w: Vec<Option<f32>> = r.columns.iter().map(|c| c.width).collect();
    assert_eq!(w, vec![None, Some(90.0), None]);
    let j = serde_json::to_value(&r).unwrap();
    assert!(
        j["columns"][0].get("width").is_none(),
        "unsized omits width"
    );
    let back: ViewResult = serde_json::from_value(j).unwrap();
    assert_eq!(back.columns[1].width, Some(90.0));
    let old =
        serde_json::json!({"kind": "table", "columns": [{"key": "a", "label": "A"}], "groups": []});
    let old: ViewResult = serde_json::from_value(old).unwrap();
    assert_eq!(old.columns[0].width, None);
}

/// Plan round-10 WP-4: filtered summary rows (totals, empty match → nulls, per group), and the
/// bracket id form keying a display name.
#[test]
fn filtered_summary_rows_and_bracket_ids() {
    let def = r#"
[properties.'note["word-count"]']
displayName = "Words"

[[views]]
type = "table"
name = "T"
filters = 'file.inFolder("daily/day/2026-10-01") || file.inFolder("daily/day/2026-10-02")'
order = ["file.name", "word-count", "note.read"]
sort = [{ property = "file.name", direction = "ASC" }]
summaries = { "word-count" = "Sum" }
columnSize = { 'note["word-count"]' = 90 }

[[views.summary_rows]]
label = "Unread"
filter = 'note.read == false'
summaries = { "word-count" = "Sum", 'note["read"]' = "Count" }

[[views.summary_rows]]
label = "None"
filter = 'note.read == "never"'
summaries = { "word-count" = "Sum" }
"#;
    let fx = fixture(&[("sr.toml", def)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let r = fx.run(&mut h, "sr#T", None);
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert_eq!(r.columns[1].label, "Words");
    assert_eq!(r.columns[1].width, Some(90.0));
    let labels: Vec<&str> = r.summaries.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["Sum", "Unread", "None"]);
    let cells = |i: usize| r.summaries[i].cells.iter().map(text).collect::<Vec<_>>();
    assert_eq!(cells(0), ["", "2200", ""]);
    // b (1000) and c (600) are `read: false`; a is true and d has no `read`.
    assert_eq!(cells(1), ["", "1600", "2"]);
    // No match: null cells, not the plain footer's zero.
    assert_eq!(cells(2), ["", "", ""]);
    assert!(matches!(r.summaries[2].cells[1], Cell::Null));
}

/// Plan round-10 WP-4: the migration-0006 output parses and its `Unread` row totals the
/// not-read rows' words and minutes.
#[test]
fn migrated_reading_list_has_an_unread_row() {
    let migrated = r##"# A day-note reading list with an `Unread` summary row.
# `this` = the day note; rows are the reading entries in `daily/day/<day>/`.

[filters]
and = ['file.folder == "daily/day/" + this.file.name']

[formulas]
tags = 'list(note.tags).map(value.toString().replace("#", "")).filter(value != "clippings" && value != "refined" && value != "using").join(", ")'
minutes = '(number(note["word-count"]) / number(file("_custom/Settings.md").properties.reading_wpm)).round(1)'
read = 'if(note.read == true, "✅", "❌")'

[[views]]
type = "table"
name = "Reading List"
order = ["file.name", "note.publication", "formula.tags", "word-count", "formula.minutes", "note.read"]
summaries = { "word-count" = "Sum", "formula.minutes" = "Sum" }
columnSize = { "note.read" = 80, "word-count" = 80, "formula.minutes" = 80 }
maxHeight = "none"

[[views.sort]]
property = "file.name"
direction = "ASC"

[[views.summary_rows]]
label = "Unread"
filter = 'note.read != true'
summaries = { "word-count" = "Sum", "formula.minutes" = "Sum" }

[properties."note.read"]
displayName = "Read"
checkbox = true

[properties."note.publication"]
displayName = "Publication"

[properties."file.name"]
displayName = "Note"

[properties."word-count"]
displayName = "Words"
"##;
    let fx = fixture(&[("daily-reading-list.toml", migrated)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let r = fx.run(
        &mut h,
        "daily-reading-list#Reading List",
        Some("daily/day/2026-10-01.md"),
    );
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert_eq!(
        r.columns
            .iter()
            .find(|c| c.key == "word-count")
            .unwrap()
            .label,
        "Words"
    );
    let unread = r
        .summaries
        .iter()
        .find(|s| s.label == "Unread")
        .expect("Unread row");
    let cells: Vec<String> = unread.cells.iter().map(text).collect();
    assert_eq!(cells, ["", "", "", "1000", "5", ""]);
}

/// WP-R6: the vocabulary the editor's view pickers list — `file.*` ids (each resolves to a
/// non-null value on a real file), the aggregates in display order, the canonical id rule.
#[test]
fn exported_picker_vocabulary() {
    let fx = fixture(&[]);
    for id in crate::FILE_PROPERTY_IDS {
        assert!(!matches!(ev(&fx, A, None, id), Value::Null), "{id}");
    }
    let labels: Vec<&str> = crate::SumFn::ALL.iter().map(|f| f.label()).collect();
    assert_eq!(labels, ["Sum", "Average", "Count", "Min", "Max"]);
    let c = crate::canonical_property_id;
    assert_eq!(c("word-count"), "note.word-count");
    assert_eq!(c("note[\"rating\"]"), "note.rating");
    assert_eq!(c("formula['age']"), "formula.age");
    assert_eq!(c("file.name"), "file.name");
}
