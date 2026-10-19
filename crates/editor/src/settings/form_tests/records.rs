//! Record lists and maps: collapse/expand, add/move/remove, step records, map keys.

use super::*;

#[test]
fn record_lists_are_collapsed_by_default_with_a_summary() {
    let st = loaded(ACTIONS, ONE_ACTION);
    let rows = rows_of(&st, ACTIONS);
    let items: Vec<&Row> = rows
        .iter()
        .filter(|r| matches!(r.kind, Kind::Item { .. }))
        .collect();
    assert_eq!(items.len(), 1);
    assert!(
        matches!(&items[0].kind, Kind::Item { summary, expanded: false, .. } if summary == "tidy")
    );
    assert!(
        !rows.iter().any(|r| r.path == p("action[0].id")),
        "collapsed: no field rows"
    );
}

#[test]
fn expanded_action_shows_fields_unknown_keys_read_only_and_steps() {
    let mut st = loaded(ACTIONS, ONE_ACTION);
    st.form_msg(ACTIONS, FormMsg::Toggle(p("action[0]")));
    st.form_msg(ACTIONS, FormMsg::Toggle(p("action[0].steps[0]")));
    let rows = rows_of(&st, ACTIONS);
    let id = rows
        .iter()
        .find(|r| r.path == p("action[0].id"))
        .expect("id");
    assert!(matches!(&id.kind, Kind::Text { value, required: true, .. } if value == "tidy"));
    let unk = rows
        .iter()
        .find(|r| r.path == p("action[0].mystery"))
        .expect("unknown");
    assert!(matches!(&unk.kind, Kind::ReadOnly { toml } if toml == "7"));
    let step = rows
        .iter()
        .find(|r| r.path == p("action[0].steps[0]"))
        .expect("step");
    assert!(
        matches!(&step.kind, Kind::Item { summary, .. } if summary.starts_with("Move")),
        "{step:?}"
    );
    assert!(
        rows.iter()
            .any(|r| matches!(&r.kind, Kind::Variant { tag: Some(t), .. } if t == "move"))
    );
    assert!(rows.iter().any(|r| r.path == p("action[0].steps[0].to")));
}

#[test]
fn record_list_add_move_remove_keeps_expansion_on_its_item() {
    let text = "[[action]]\nid = \"a\"\n\n[[action]]\nid = \"b\"\n";
    let mut st = loaded(ACTIONS, text);
    let list = p("action");
    st.form_msg(ACTIONS, FormMsg::Toggle(p("action[1]")));
    st.edit(
        ACTIONS,
        &DocEdit::InsertRecord(
            list.clone(),
            2,
            vec![("id".into(), ScalarValue::Str("c".into()))],
        ),
    )
    .expect("add");
    let ids = |st: &Settings| {
        let d = st.file(ACTIONS).and_then(|f| f.doc()).expect("doc");
        (0..d.len(&list))
            .map(|i| d.get_str(&list.index(i).key("id")).unwrap_or("").to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&st), ["a", "b", "c"]);
    assert!(
        st.form.is_expanded(ACTIONS, &p("action[2]")),
        "new item opens"
    );
    st.edit(ACTIONS, &DocEdit::Move(list.clone(), 1, 0))
        .expect("up");
    assert_eq!(ids(&st), ["b", "a", "c"]);
    assert!(
        st.form.is_expanded(ACTIONS, &p("action[0]")),
        "b's expansion moved"
    );
    st.edit(ACTIONS, &DocEdit::Remove(p("action[1]")))
        .expect("rm");
    assert_eq!(ids(&st), ["b", "c"]);
    assert!(
        st.form.is_expanded(ACTIONS, &p("action[1]")),
        "c shifted down"
    );
    assert!(st.file(ACTIONS).is_some_and(|f| f.dirty()));
}

#[test]
fn add_step_inserts_a_tagged_record() {
    let text = "[[action]]\nid = \"a\"\n";
    let mut st = loaded(ACTIONS, text);
    st.form_msg(ACTIONS, FormMsg::Toggle(p("action[0]")));
    let rows = rows_of(&st, ACTIONS);
    let add = rows
        .iter()
        .find(|r| matches!(&r.kind, Kind::AddItem { list, union: Some(_), .. } if *list == p("action[0].steps")))
        .expect("add step row");
    let Kind::AddItem {
        list,
        len,
        union: Some(u),
    } = &add.kind
    else {
        panic!()
    };
    st.edit(
        ACTIONS,
        &DocEdit::InsertRecord(
            list.clone(),
            *len,
            vec![(u.tag.to_owned(), ScalarValue::Str("lint".into()))],
        ),
    )
    .expect("insert");
    let d = st.file(ACTIONS).and_then(|f| f.doc()).expect("doc");
    assert_eq!(d.get_str(&p("action[0].steps[0].type")), Some("lint"));
}

#[test]
fn map_key_rename_keeps_value_comment_and_expansion() {
    let text = "[kinds.book] # books\nfolder = \"books\"\n\n[kinds.film]\nfolder = \"films\"\n";
    let mut st = loaded(KINDS, text);
    st.form_msg(KINDS, FormMsg::Toggle(p("kinds.book")));
    st.form_msg(
        KINDS,
        FormMsg::RenameDraft {
            at: p("kinds.book"),
            text: "novel".into(),
        },
    );
    st.form_msg(
        KINDS,
        FormMsg::RenameCommit {
            map: p("kinds"),
            from: "book".into(),
        },
    );
    let t = doc_text(&st, KINDS);
    assert!(t.contains("[kinds.novel] # books"), "{t}");
    assert!(t.find("novel") < t.find("film"), "position kept: {t}");
    assert!(st.form.is_expanded(KINDS, &p("kinds.novel")));
    let rows = rows_of(&st, KINDS);
    assert!(
        rows.iter()
            .any(|r| matches!(&r.kind, Kind::MapKey { key, .. } if key == "novel"))
    );
}

#[test]
fn map_add_and_remove_keys() {
    let mut st = loaded(KINDS, "[kinds.book]\nfolder = \"b\"\n");
    st.form_msg(
        KINDS,
        FormMsg::NewKeyDraft {
            map: p("kinds"),
            text: "film".into(),
        },
    );
    st.form_msg(
        KINDS,
        FormMsg::NewKeyAdd {
            map: p("kinds"),
            value: &Node::FreeTable,
        },
    );
    let d = st.file(KINDS).and_then(|f| f.doc()).expect("doc");
    assert_eq!(d.keys(&p("kinds")), ["book", "film"]);
    st.edit(KINDS, &DocEdit::Remove(p("kinds.book")))
        .expect("rm");
    let d = st.file(KINDS).and_then(|f| f.doc()).expect("doc");
    assert_eq!(d.keys(&p("kinds")), ["film"]);
    // A duplicate key is refused with a notice.
    assert!(
        st.edit(
            KINDS,
            &DocEdit::InsertKey(p("kinds"), "film".into(), &Node::FreeTable)
        )
        .is_err()
    );
    assert!(st.file(KINDS).is_some_and(|f| f.notice().is_some()));
}
