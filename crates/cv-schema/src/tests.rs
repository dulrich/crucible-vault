use toml_edit::{Item, Table, Value, value};

use super::*;

const ACTIONS: &str = r#"# Top comment survives.
unknown_root = "kept"

# First action.
[[action]]
id = "a" # trailing comment
label = "A"
mystery = 1
[[action.steps]]
type = "lint" # lint step

# Second action.
[[action]]
id = "b"
keys = ["Ctrl+B", "Ctrl+Shift+B"]
[[action.steps]]
type = "guard"
tag = "x"

[other]
z = 1
"#;

fn doc(s: &str) -> ConfigDoc {
    ConfigDoc::parse(s).unwrap()
}

fn p(s: &str) -> Path {
    Path::parse(s).unwrap()
}

fn errs(text: &str, file: &str) -> Vec<String> {
    validate(&doc(text), schema_for(file).unwrap())
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn round_trip_is_byte_identical() {
    assert_eq!(doc(ACTIONS).to_string(), ACTIONS);
}

#[test]
fn path_parse_and_display() {
    let path = p(r#"action[0].steps[1]."a.b"[2]"#);
    assert_eq!(path.0.len(), 6);
    assert_eq!(path.to_string(), r#"action[0].steps[1]."a.b"[2]"#);
    assert!(Path::parse("a.").is_err());
    assert!(Path::parse("a[x]").is_err());
    assert!(Path::parse("\"open").is_err());
}

#[test]
fn typed_reads() {
    let d = doc(ACTIONS);
    assert_eq!(d.get_str(&p("action[0].id")), Some("a"));
    assert_eq!(d.get_str(&p("action[1].steps[0].tag")), Some("x"));
    assert_eq!(
        d.get_str_list(&p("action[1].keys")).unwrap(),
        vec!["Ctrl+B", "Ctrl+Shift+B"]
    );
    assert_eq!(
        d.get_str_list(&p("action[1].steps[0].tag")).unwrap(),
        vec!["x"]
    );
    assert_eq!(d.get_int(&p("other.z")), Some(1));
    assert_eq!(d.get_float(&p("other.z")), Some(1.0));
    assert_eq!(d.len(&p("action")), 2);
    assert!(d.get(&p("action[5]")).is_none());
    assert_eq!(d.keys(&Path::root())[0], "unknown_root");
}

#[test]
fn set_existing_keeps_comments_and_unknowns() {
    let mut d = doc(ACTIONS);
    d.set(&p("action[0].id"), "renamed").unwrap();
    let out = d.to_string();
    assert!(out.contains("id = \"renamed\" # trailing comment"), "{out}");
    assert_eq!(
        out,
        ACTIONS.replace("id = \"a\" # trailing", "id = \"renamed\" # trailing")
    );
}

#[test]
fn set_new_key_and_missing_parent() {
    let mut d = doc(ACTIONS);
    d.set(&p("action[0].durable"), false).unwrap();
    d.set(&p("action[1].resources.note"), false).unwrap();
    let out = d.to_string();
    assert!(out.contains("# First action."));
    assert!(out.contains("mystery = 1"));
    assert!(out.contains("unknown_root = \"kept\""));
    let back = doc(&out);
    assert_eq!(back.get_bool(&p("action[0].durable")), Some(false));
    assert_eq!(back.get_bool(&p("action[1].resources.note")), Some(false));
}

#[test]
fn set_inside_inline_table_stays_inline() {
    let mut d =
        doc("[[action]]\nid = \"a\"\nstarters = [{ type = \"event\", kinds = [\"created\"] }]\n");
    d.set(&p("action[0].starters[0].selector.folder"), "inbox")
        .unwrap();
    let out = d.to_string();
    assert!(out.contains("selector = { folder = \"inbox\" }"), "{out}");
}

#[test]
fn set_list_item() {
    let mut d = doc(ACTIONS);
    d.set(&p("action[1].keys[1]"), "Alt+B").unwrap();
    assert!(d.to_string().contains(r#"keys = ["Ctrl+B", "Alt+B"]"#));
    assert!(matches!(
        d.set(&p("action[1].keys[9]"), "x"),
        Err(DocError::OutOfRange(_))
    ));
}

#[test]
fn insert_record_into_array_of_tables() {
    let mut d = doc(ACTIONS);
    let mut t = Table::new();
    t.insert("id", value("c"));
    d.insert_record(&p("action"), 1, t).unwrap();
    let out = d.to_string();
    let back = doc(&out);
    assert_eq!(back.get_str(&p("action[1].id")), Some("c"));
    assert_eq!(back.get_str(&p("action[2].id")), Some("b"));
    assert!(out.contains("# Second action."));
    assert!(out.contains("[other]"));
    assert_eq!(back.get_int(&p("other.z")), Some(1));
}

#[test]
fn insert_record_creates_list_and_inline_variant() {
    let mut d = doc("# c\n");
    let mut t = Table::new();
    t.insert("name", value("n"));
    t.insert("path", value("p.md"));
    d.insert_record(&p("bookmark"), 0, t.clone()).unwrap();
    assert!(d.to_string().contains("[[bookmark]]"));
    let mut d = doc("[[provider]]\nid = \"x\"\nkind = \"ollama\"\nmodels = [{ id = \"m\" }]\n");
    let mut m = Table::new();
    m.insert("id", value("n"));
    d.insert_record(&p("provider[0].models"), 1, m).unwrap();
    assert!(
        d.to_string()
            .contains(r#"models = [{ id = "m" }, { id = "n" }]"#),
        "{d}"
    );
}

#[test]
fn insert_value_into_inline_array() {
    let mut d = doc(ACTIONS);
    d.insert_value(&p("action[1].keys"), 0, "F1").unwrap();
    d.insert_value(&p("action[0].needs"), 0, "line").unwrap();
    let out = d.to_string();
    assert!(
        out.contains(r#"keys = ["F1", "Ctrl+B", "Ctrl+Shift+B"]"#),
        "{out}"
    );
    assert_eq!(
        doc(&out).get_str_list(&p("action[0].needs")).unwrap(),
        vec!["line"]
    );
}

#[test]
fn remove_key_and_items() {
    let mut d = doc(ACTIONS);
    d.remove(&p("action[0].label")).unwrap();
    d.remove(&p("action[1].keys[0]")).unwrap();
    d.remove(&p("action[0]")).unwrap();
    let out = d.to_string();
    let back = doc(&out);
    assert_eq!(back.len(&p("action")), 1);
    assert_eq!(
        back.get_str_list(&p("action[0].keys")).unwrap(),
        vec!["Ctrl+Shift+B"]
    );
    assert!(out.contains("# Top comment survives."));
    assert!(out.contains("# Second action."));
    assert!(matches!(d.remove(&p("nope")), Err(DocError::NotFound(_))));
}

#[test]
fn move_array_of_tables_entry_moves_its_comment_and_subtables() {
    let mut d = doc(ACTIONS);
    d.move_item(&p("action"), 1, 0).unwrap();
    let out = d.to_string();
    let b = out.find("# Second action.").unwrap();
    let a = out.find("# First action.").unwrap();
    assert!(b < a, "{out}");
    let back = doc(&out);
    assert_eq!(back.get_str(&p("action[0].id")), Some("b"));
    assert_eq!(back.get_str(&p("action[0].steps[0].type")), Some("guard"));
    assert_eq!(back.get_str(&p("action[1].steps[0].type")), Some("lint"));
    assert!(
        out.trim_end().ends_with("z = 1"),
        "[other] stays last: {out}"
    );
    d.move_item(&p("action"), 0, 1).unwrap();
    assert_eq!(d.to_string(), ACTIONS);
}

#[test]
fn move_inline_array_item() {
    let mut d = doc(ACTIONS);
    d.move_item(&p("action[1].keys"), 1, 0).unwrap();
    assert!(
        d.to_string()
            .contains(r#"keys = ["Ctrl+Shift+B", "Ctrl+B"]"#)
    );
    assert!(d.move_item(&p("action[1].keys"), 0, 5).is_err());
}

const KINDS: &str = "# kinds\n[kinds.alpha]\nlabel = \"A\" # a\n\n# beta comment\n[kinds.beta]\nlabel = \"B\"\n\n[kinds.gamma]\nlabel = \"G\"\n";

#[test]
fn rename_map_key_keeps_position_and_comments() {
    let mut d = doc(KINDS);
    d.rename_key(&p("kinds"), "beta", "bravo").unwrap();
    let out = d.to_string();
    assert_eq!(out, KINDS.replace("[kinds.beta]", "[kinds.bravo]"));
    assert!(matches!(
        d.rename_key(&p("kinds"), "alpha", "gamma"),
        Err(DocError::Exists(_))
    ));
    assert!(matches!(
        d.rename_key(&p("kinds"), "zzz", "y"),
        Err(DocError::NotFound(_))
    ));
}

#[test]
fn rename_root_and_inline_keys() {
    let mut d = doc("# agents\n[a1]\nmax_tokens = 1\n[a2]\nmax_tokens = 2\n");
    d.rename_key(&Path::root(), "a1", "first").unwrap();
    assert_eq!(d.keys(&Path::root()), vec!["first", "a2"]);
    assert!(d.to_string().starts_with("# agents\n[first]"));
    let mut d = doc("[x]\nsteps = { a = true, b = false }\n");
    d.rename_key(&p("x.steps"), "a", "c").unwrap();
    assert!(
        d.to_string().contains("steps = { c = true, b = false }"),
        "{d}"
    );
}

#[test]
fn insert_and_remove_map_key() {
    let mut d = doc(KINDS);
    let mut t = Table::new();
    t.insert("label", value("D"));
    d.insert_key(&p("kinds"), "delta", Item::Table(t)).unwrap();
    assert!(matches!(
        d.insert_key(&p("kinds"), "delta", value(1)),
        Err(DocError::Exists(_))
    ));
    d.remove(&p("kinds.alpha")).unwrap();
    let out = d.to_string();
    assert!(out.contains("[kinds.delta]"));
    assert!(!out.contains("[kinds.alpha]"));
    assert!(out.contains("# beta comment"));
    let mut e = ConfigDoc::new();
    e.insert_key(
        &p("keys"),
        "open-settings",
        Item::Value(Value::Array(toml_edit::Array::new())),
    )
    .unwrap();
    assert!(e.to_string().contains("open-settings = []"), "{e}");
}

#[test]
fn documented_exception_trailing_newline() {
    let mut d = doc("a = 1");
    d.set(&p("b"), 2).unwrap();
    let out = d.to_string();
    assert!(out.starts_with("a = 1"));
    assert_eq!(doc(&out).get_int(&p("b")), Some(2));
}

#[test]
fn parse_error_is_reported() {
    assert!(matches!(ConfigDoc::parse("[a"), Err(DocError::Parse(_))));
}

#[test]
fn validate_required_type_enum() {
    let e = errs(
        "[[action]]\nlabel = 1\nlane = \"fast\"\n[[action.steps]]\ntype = \"bogus\"\n",
        "actions.toml",
    );
    assert!(
        e.iter().any(|m| m == "action[0].id: Id is required"),
        "{e:?}"
    );
    assert!(
        e.iter().any(|m| m == "action[0].label: must be a string"),
        "{e:?}"
    );
    assert!(
        e.iter()
            .any(|m| m.starts_with("action[0].lane: \"fast\" is not one of")),
        "{e:?}"
    );
    assert!(
        e.iter().any(|m| m.contains("unknown step type \"bogus\"")),
        "{e:?}"
    );
}

#[test]
fn validate_ranges_int_and_float() {
    assert!(errs("font_size = 22\nui_scale = 1\n", "editor.toml").is_empty());
    let e = errs(
        "font_size = 4.0\nui_scale = 9\n[search]\nlimit = 0\n",
        "editor.toml",
    );
    assert_eq!(e.len(), 3, "{e:?}");
    assert!(e[0].contains("between 8 and 64"), "{e:?}");
    let e = errs(
        "[[action]]\nid = \"a\"\n[[action.steps]]\ntype = \"localize\"\nimage_quality = 80\ntimeout_secs = 0\n",
        "actions.toml",
    );
    assert_eq!(
        e,
        vec!["action[0].steps[0].timeout_secs: must be at least 1"]
    );
}

#[test]
fn validate_exactly_one_of() {
    assert!(
        errs(
            "[[schedule]]\naction = \"a\"\ndaily = \"07:00\"\n",
            "schedules.toml"
        )
        .is_empty()
    );
    for bad in [
        "[[schedule]]\naction = \"a\"\n",
        "[[schedule]]\naction = \"a\"\ndaily = \"07:00\"\nevery = \"1h\"\n",
    ] {
        let e = errs(bad, "schedules.toml");
        assert_eq!(
            e,
            vec!["schedule[0]: set exactly one of `daily`, `every`"],
            "{bad}"
        );
    }
}

#[test]
fn validate_mutual_exclusion_and_at_least_one() {
    let e = errs(
        "[a]\nsystem = \"s\"\nsystem_file = \"f.md\"\nmax_tokens = 5\n",
        "agents.toml",
    );
    assert_eq!(e, vec!["a: set at most one of `system`, `system_file`"]);
    let e = errs(
        "[[action]]\nid = \"a\"\n[[action.steps]]\ntype = \"move\"\n",
        "actions.toml",
    );
    assert_eq!(
        e,
        vec!["action[0].steps[0]: set at least one of `to`, `kind`"]
    );
}

#[test]
fn validate_string_or_list_and_choices() {
    assert!(errs("[[action]]\nid = \"a\"\n[[action.steps]]\ntype = \"guard\"\ntag = \"x\"\nnot_tag = [\"y\"]\n", "actions.toml").is_empty());
    let e = errs(
        "[[action]]\nid = \"a\"\nneeds = \"line\"\n[[action.steps]]\ntype = \"guard\"\n",
        "actions.toml",
    );
    assert_eq!(e, vec!["action[0].needs: must be a list of strings"]);
    let e = errs(
        "[[provider]]\nid = \"a\"\nkind = \"ollama\"\nmodels = [{ id = \"m\", capabilities = [\"fly\"] }]\n",
        "providers.toml",
    );
    assert_eq!(e.len(), 1, "{e:?}");
}

#[test]
fn validate_view_filters() {
    let ok = "[filters]\nand = ['file.inFolder(\"x\")', { or = [\"a\", { not = [\"b\"] }] }]\n[[views]]\ntype = \"table\"\nname = \"T\"\n";
    assert!(errs(ok, "views/x.toml").is_empty());
    let e = errs("[filters]\nand = [1]\nor = []\n", "views/x.toml");
    assert_eq!(
        e,
        vec!["filters: a filter table needs exactly one of and, or, not"]
    );
}

#[test]
fn lookup_by_file_and_step_kind() {
    for f in FILES {
        assert!(schema_for(f).is_some(), "{f}");
    }
    assert!(schema_for("nope.toml").is_none());
    assert_eq!(schema_for("views/a.toml"), Some(view_schema()));
    assert_eq!(schema_for("editor.toml"), Some(editor_schema()));
    assert_eq!(step_schema("agent").unwrap().name, "agent");
    assert!(step_schema("nope").is_none());
    assert_eq!(schemas::STEP.variants.len(), 15);
}

#[test]
fn coverage_paths() {
    let all = schema_paths(schema_for("schedules.toml").unwrap());
    assert_eq!(
        all.into_iter().collect::<Vec<_>>(),
        vec![
            "schedule",
            "schedule[].action",
            "schedule[].daily",
            "schedule[].every"
        ]
    );
    let d = doc("[[schedule]]\naction = \"a\"\nevery = \"1h\"\n");
    let got = covered_paths(&d, schema_for("schedules.toml").unwrap());
    assert!(got.contains("schedule[].every") && !got.contains("schedule[].daily"));
    let steps = schema_paths(schema_for("actions.toml").unwrap());
    assert!(steps.contains("action[].steps[]<type=agent>.output_var"));
}

#[test]
fn defaults_are_valid_values_of_their_nodes() {
    use node::Default as D;
    fn walk(n: &Node, seen: &mut usize) {
        match *n {
            Node::Record(r) => {
                for f in r.fields {
                    if let Some(d) = f.default {
                        *seen += 1;
                        let item = match d {
                            D::Str(s) => value(s),
                            D::Bool(b) => value(b),
                            D::Int(i) => value(i),
                            D::Float(x) => value(x),
                            D::StrList(l) => value(l.iter().copied().collect::<toml_edit::Array>()),
                        };
                        let e = validate_item(&item, &f.node, &Path::root());
                        assert!(e.is_empty(), "{}: {e:?}", f.key);
                    }
                    walk(&f.node, seen);
                }
            }
            Node::Union(u) => u
                .variants
                .iter()
                .for_each(|v| walk(&Node::Record(v.record), seen)),
            Node::List(x) | Node::Map(x) => walk(x, seen),
            _ => {}
        }
    }
    let mut seen = 0;
    for f in FILES {
        walk(schema_for(f).unwrap(), &mut seen);
    }
    walk(view_schema(), &mut seen);
    assert!(seen > 30, "{seen}");
}
