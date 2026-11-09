use super::*;

#[test]
fn function_and_field_vectors() {
    let fx = fixture(&[]);
    let cases: &[(Option<&str>, Option<&str>, &str, &str)] = &[
        (A, None, "file.name", "a"),
        (A, None, "file.path", "daily/day/2026-10-01/a.md"),
        (A, None, "file.folder", "daily/day/2026-10-01"),
        (A, None, "file.ext", "md"),
        (A, None, "file.size > 0", "true"),
        (A, None, "file.mtime > date(\"2000-01-01\")", "true"),
        (A, None, "file.ctime > date(\"2000-01-01\")", "true"),
        (A, None, "file.tags.join(\" \")", "#blog #clippings"),
        (Some("notes/x.md"), None, "file.links.length", "2"),
        (
            Some("notes/x.md"),
            None,
            "file.links[0].path",
            "stack/tool1.md",
        ),
        (
            Some("stack/tool1.md"),
            None,
            "file.backlinks.map(value.path).join(\",\")",
            "notes/x.md,stack/tool2.md",
        ),
        (
            Some("notes/x.md"),
            None,
            "file.hasLink(\"stack/tool1.md\")",
            "true",
        ),
        (
            Some("notes/x.md"),
            Some("stack/tool1.md"),
            "file.hasLink(this.file)",
            "true",
        ),
        (
            A,
            Some("daily/day/2026-10-01.md"),
            "this.file.name",
            "2026-10-01",
        ),
        (
            A,
            Some("daily/day/2026-10-01.md"),
            "file.folder == \"daily/day/\" + this.file.name",
            "true",
        ),
        (
            A,
            Some("core/works/Ann Blog.md"),
            "this[\"author-filter\"]",
            "Ann",
        ),
        (
            A,
            Some("core/works/Ann Blog.md"),
            "this.file.properties[\"work-status\"]",
            "active",
        ),
        (A, None, "note.read", "true"),
        (A, None, "note[\"word-count\"] + 1", "401"),
        (A, None, "read", "true"),
        (A, None, "formula.minutes", "2"),
        (A, None, "formula.nope", ""),
        (A, None, "formula.loop", ""),
        (
            A,
            None,
            "file(\"_custom/Settings.md\").properties.reading_wpm",
            "200",
        ),
        (
            A,
            None,
            "file(\"_custom/Settings\").path",
            "_custom/Settings.md",
        ),
        (A, None, "if(file(\"nope.md\"), 1, 2)", "2"),
        (A, None, "if(note.read == true, \"y\", \"n\")", "y"),
        (A, None, "if(false, 1)", ""),
        (A, None, "list(note.tags).length", "2"),
        (A, None, "list(note.author).length", "1"),
        (A, None, "list(note.nothing).length", "0"),
        (A, None, "note.author.toString()", "Ann"),
        (A, None, "\"  x \".trim()", "x"),
        (A, None, "\"#a#b\".replace(\"#\", \"\")", "ab"),
        (
            A,
            None,
            "list(note.tags).map(value.toString().upper()).join(\"/\")",
            "BLOG/CLIPPINGS",
        ),
        (
            A,
            None,
            "list(note.tags).filter(value != \"clippings\")",
            "blog",
        ),
        (A, None, "list(note.tags).contains(\"blog\")", "true"),
        (A, None, "\"abc\".contains(\"bc\")", "true"),
        (A, None, "file.path.startsWith(\"daily/\")", "true"),
        (A, None, "file.name.endsWith(\"a\")", "true"),
        (A, None, "number(\"2.5\") * 2", "5"),
        (A, None, "number(note.missing)", "0"),
        (A, None, "(10 / 3).round(2)", "3.33"),
        (A, None, "(2.5).round()", "3"),
        (A, None, "(1 / 3).toFixed(1)", "0.3"),
        (A, None, "[1, 2, 3].join(\"-\")", "1-2-3"),
        (A, None, "file.inFolder(\"daily/day\")", "true"),
        (A, None, "file.inFolder(\"daily/da\")", "false"),
        (A, None, "file.hasTag(\"blog\")", "true"),
        (A, None, "file.hasTag(\"#BLOG\", \"zzz\")", "true"),
        (A, None, "file.hasTag(\"blo\")", "false"),
        (
            Some("daily/day/2026-10-03/e.md"),
            None,
            "file.hasTag(\"using\")",
            "true",
        ),
        (A, None, "link(file.path, \"L\")", "L"),
        (A, None, "link(\"x/y.md\")", "y"),
        (A, None, "link(\"x/y.md\").path", "x/y.md"),
        (A, None, "file.asLink().path", "daily/day/2026-10-01/a.md"),
        (A, None, "\"a/b/c\".split(\"/\")[1]", "b"),
        (A, None, "\"a/b/c\".split(\"/\")[-1]", "c"),
        (A, None, "\"a/b/c\".split(\"/\").length", "3"),
        (A, None, "\"ABC\".lower()", "abc"),
        (A, None, "\"abc\".length", "3"),
        (A, None, "today()", "2026-10-03"),
        (A, None, "today().format(\"YYYY-MM\")", "2026-10"),
        (A, None, "note.created", "2026-10-01"),
        (A, None, "note.created.format(\"DD/MM/YYYY\")", "01/10/2026"),
        (A, None, "note.created < today()", "true"),
        (A, None, "\"2026-10-02\" < today()", "true"),
        (A, None, "date(\"2026-10-03\") < today()", "false"),
        (A, None, "note.created == \"2026-10-01\"", "true"),
        (A, None, "today().year", "2026"),
        (A, None, "files(\"core/works\").length", "2"),
        (
            A,
            None,
            "files(\"core/works\").map(value.name).join(\",\")",
            "Ann Blog,Empty",
        ),
        (
            A,
            None,
            "files(\"core/works\").filter(value.properties[\"author-filter\"]).length",
            "1",
        ),
        (
            A,
            None,
            "files(\"core/works\").filter(list(value.properties[\"source-prefix-filter\"]).filter(note.source.startsWith(value)).length > 0).map(value.name).join(\",\")",
            "Ann Blog",
        ),
        (
            A,
            None,
            "list(\"x\").map(list(\"y\").map(outer + value).join(\"\")).join(\"\")",
            "xy",
        ),
        (
            Some("projects/alpha/PROJECT.md"),
            None,
            "file.tasks.filter(value.status == \" \" && value.heading == \"Blocked on User\").length",
            "1",
        ),
        (
            Some("projects/alpha/PROJECT.md"),
            None,
            "list(note[\"pending-items\"]).filter(value.toString().trim() != \"\").length",
            "1",
        ),
        (A, None, "!true || 1 == 1 && 2 != 3", "true"),
        (A, None, "-2 + 5 % 3 + 1 >= 0", "true"),
        (A, None, "\"n\" + 1", "n1"),
        (A, None, "null", ""),
    ];
    for (row, this, src, want) in cases {
        assert_eq!(&t(&fx, *row, *this, src), want, "{src}");
    }
    assert!(matches!(
        ev(&fx, A, None, "\"\".isEmpty()"),
        Value::Bool(true)
    ));
    assert!(matches!(ev(&fx, A, None, "now()"), Value::Date(_, false)));
    assert!(matches!(ev(&fx, None, None, "file.name"), Value::Null));
}

#[test]
fn every_base_vector_evaluates_without_panic() {
    let fx = fixture(&[]);
    for v in crate::expr::tests_support::BASE_VECTORS {
        for row in [A, Some("projects/alpha/PROJECT.md"), None] {
            let _ = ev(&fx, row, Some("core/works/Ann Blog.md"), v);
        }
    }
    // A project-list formula on a fixture project.
    assert_eq!(
        t(
            &fx,
            Some("projects/alpha/PROJECT.md"),
            None,
            crate::expr::tests_support::BASE_VECTORS[16]
        ),
        "Alpha"
    );
    assert_eq!(
        t(
            &fx,
            Some("projects/beta/PROJECT.md"),
            None,
            crate::expr::tests_support::BASE_VECTORS[16]
        ),
        "beta"
    );
    assert_eq!(
        t(
            &fx,
            Some("projects/beta/PROJECT.md"),
            None,
            crate::expr::tests_support::BASE_VECTORS[20]
        ),
        "blocked"
    );
    assert_eq!(
        t(
            &fx,
            Some("projects/alpha/PROJECT.md"),
            None,
            crate::expr::tests_support::BASE_VECTORS[18]
        ),
        "README"
    );
    // The blog-story-list author/prefix filter with `this` = a work.
    let w = Some("core/works/Ann Blog.md");
    let f = crate::expr::tests_support::BASE_VECTORS[6];
    assert_eq!(t(&fx, A, w, f), "true");
    assert_eq!(t(&fx, Some("daily/day/2026-10-02/c.md"), w, f), "true");
    assert_eq!(t(&fx, Some("daily/day/2026-10-01/b.md"), w, f), "false");
    assert_eq!(t(&fx, A, Some("core/works/Empty.md"), f), "false");
}
