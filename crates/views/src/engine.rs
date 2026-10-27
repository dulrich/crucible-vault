//! Runs one compiled view: SQL narrows the candidate files (top-level conjuncts of the form
//! `file.folder|name|path|ext == <const>`, `file.inFolder(<const>)`, `file.hasTag(<const>…)`,
//! `file.folder|name|path.startsWith(<const>)`, `note.k == <const text|bool>`), then every filter
//! is re-checked in Rust, so SQL only ever narrows — correctness never depends on it.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::types::Value as SqlValue;

use crate::db::{FileInfo, like_escape};
use crate::def::{Aggregate, Direction, Filter, SumFn, TaskGroup, View, ViewType};
use crate::eval::{Env, RowCtx, task_value};
use crate::expr::{BinOp, Expr, Meth};
use crate::result::{Cell, Column, ColumnRole, Group, Kind, Row, Summary, ViewResult};

/// The column id (as written in `order`) of a record source's task-status field.
const STATUS_COLUMN: &str = "status";
use crate::value::{Value, parse_date, sort_cmp};

type R<T> = rusqlite::Result<T>;

/// Candidate count above which props/tags are bulk-loaded.
const PRELOAD_AT: usize = 32;

/// The compiled SQL narrowing: a `WHERE` suffix (`AND …` clauses over alias `f`) and its args.
#[derive(Debug, Default)]
pub struct Narrow {
    pub sql: String,
    pub args: Vec<SqlValue>,
}

fn file_col(e: &Expr) -> Option<&'static str> {
    match e {
        Expr::Member(r, c) if matches!(&**r, Expr::Ident(n) if n == "file") => match c.as_str() {
            "path" => Some("path"),
            "folder" => Some("folder"),
            "name" => Some("name"),
            "ext" => Some("ext"),
            _ => None,
        },
        _ => None,
    }
}

fn is_file_ident(e: &Expr) -> bool {
    matches!(e, Expr::Ident(n) if n == "file")
}

fn note_key(e: &Expr) -> Option<&str> {
    match e {
        Expr::Member(r, k) if matches!(&**r, Expr::Ident(n) if n == "note") => Some(k),
        Expr::Index(r, k) if matches!(&**r, Expr::Ident(n) if n == "note") => match &**k {
            Expr::Lit(Value::Str(s)) => Some(s),
            _ => None,
        },
        _ => None,
    }
}

fn conjuncts<'e>(f: &'e Filter, out: &mut Vec<&'e Expr>) {
    match f {
        Filter::And(v) => v.iter().for_each(|x| conjuncts(x, out)),
        Filter::Expr(e) => split_and(e, out),
        Filter::Or(_) | Filter::Not(_) => {}
    }
}

fn split_and<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    if let Expr::Bin(BinOp::And, a, b) = e {
        split_and(a, out);
        split_and(b, out);
    } else {
        out.push(e);
    }
}

/// Compiles the narrowable conjuncts of `filters` (row-independent sides are evaluated with
/// `ctx`, which has no row bound).
pub fn narrow(filters: &[Filter], ctx: &RowCtx<'_>) -> R<Narrow> {
    let mut terms = Vec::new();
    filters.iter().for_each(|f| conjuncts(f, &mut terms));
    let mut n = Narrow::default();
    let konst = |e: &Expr| -> R<Option<Value>> {
        if e.row_dependent() {
            Ok(None)
        } else {
            ctx.eval(e).map(Some)
        }
    };
    for t in terms {
        match t {
            Expr::Bin(BinOp::Eq, a, b) => {
                let (lhs, rhs) = if file_col(a).is_some() || note_key(a).is_some() {
                    (a, b)
                } else {
                    (b, a)
                };
                if let Some(col) = file_col(lhs) {
                    if let Some(Value::Str(s)) = konst(rhs)? {
                        n.sql.push_str(&format!(" AND f.{col} = ?"));
                        n.args.push(s.into());
                    }
                } else if let Some(k) = note_key(lhs) {
                    let json = match konst(rhs)? {
                        Some(Value::Str(s)) if parse_date(&s).is_none() => {
                            serde_json::to_string(&s).ok()
                        }
                        Some(Value::Bool(b)) => Some(b.to_string()),
                        _ => None,
                    };
                    if let Some(j) = json {
                        n.sql.push_str(" AND EXISTS(SELECT 1 FROM props p WHERE p.file_id = f.id AND p.key = ? AND p.value = ?)");
                        n.args.push(k.to_string().into());
                        n.args.push(j.into());
                    }
                }
            }
            Expr::Method(r, Meth::InFolder, a) if is_file_ident(r) => {
                if let Some(Some(v)) = a.first().map(konst).transpose()? {
                    let folder = v.to_text().trim_matches('/').to_string();
                    if !folder.is_empty() {
                        n.sql
                            .push_str(" AND (f.folder = ? OR f.folder LIKE ? ESCAPE '\\')");
                        n.args.push(folder.clone().into());
                        n.args.push(format!("{}/%", like_escape(&folder)).into());
                    }
                }
            }
            Expr::Method(r, Meth::HasTag, a) if is_file_ident(r) => {
                let mut tags = Vec::new();
                for x in a {
                    match konst(x)? {
                        Some(v) => tags.push(v.to_text().trim_start_matches('#').to_lowercase()),
                        None => {
                            tags.clear();
                            break;
                        }
                    }
                }
                if !tags.is_empty() && tags.iter().all(|t| !t.is_empty()) {
                    let ors = vec!["lower(t.tag) = ? OR t.tag LIKE ? ESCAPE '\\'"; tags.len()];
                    n.sql.push_str(&format!(
                        " AND EXISTS(SELECT 1 FROM tags t WHERE t.file_id = f.id AND ({}))",
                        ors.join(" OR ")
                    ));
                    for t in tags {
                        n.args.push(t.clone().into());
                        n.args.push(format!("{}/%", like_escape(&t)).into());
                    }
                }
            }
            Expr::Method(r, Meth::StartsWith, a) => {
                if let (Some(col), Some(Some(Value::Str(s)))) =
                    (file_col(r), a.first().map(konst).transpose()?)
                {
                    n.sql.push_str(&format!(" AND f.{col} LIKE ? ESCAPE '\\'"));
                    n.args.push(format!("{}%", like_escape(&s)).into());
                }
            }
            _ => {}
        }
    }
    Ok(n)
}

fn passes(ctx: &RowCtx<'_>, f: &Filter) -> R<bool> {
    Ok(match f {
        Filter::Expr(e) => ctx.eval(e)?.truthy(),
        Filter::And(v) => {
            for x in v {
                if !passes(ctx, x)? {
                    return Ok(false);
                }
            }
            true
        }
        Filter::Or(v) => {
            for x in v {
                if passes(ctx, x)? {
                    return Ok(true);
                }
            }
            false
        }
        Filter::Not(v) => {
            for x in v {
                if passes(ctx, x)? {
                    return Ok(false);
                }
            }
            true
        }
    })
}

fn summarize(f: SumFn, vals: &[&Value]) -> Value {
    let nums: Vec<f64> = vals
        .iter()
        .filter(|v| !matches!(v, Value::Null))
        .map(|v| v.to_num())
        .filter(|n| n.is_finite())
        .collect();
    #[allow(clippy::cast_precision_loss)]
    match f {
        // fold from +0.0: an empty f64 `Sum` is -0.0 and would render `-0`.
        SumFn::Sum => Value::Num(nums.iter().fold(0.0, |a, n| a + n)),
        SumFn::Count => {
            Value::Num(vals.iter().filter(|v| !matches!(v, Value::Null)).count() as f64)
        }
        SumFn::Average if nums.is_empty() => Value::Null,
        SumFn::Average => Value::Num(nums.iter().sum::<f64>() / nums.len() as f64),
        SumFn::Min => nums
            .iter()
            .copied()
            .reduce(f64::min)
            .map_or(Value::Null, Value::Num),
        SumFn::Max => nums
            .iter()
            .copied()
            .reduce(f64::max)
            .map_or(Value::Null, Value::Num),
    }
}

fn same_prop(a: &str, b: &str) -> bool {
    crate::def::norm(a) == crate::def::norm(b)
}

struct Rec {
    file: Option<Arc<FileInfo>>,
    key: Option<String>,
    /// Plan round-13 WP-4: [`Row::select`].
    select: Option<String>,
    cells: Vec<Value>,
    sort: Vec<Value>,
    group: Value,
    /// Per `view.summary_rows` entry: does this row match its filter.
    flags: Vec<bool>,
}

fn ordered(dir: Direction, o: std::cmp::Ordering) -> std::cmp::Ordering {
    if dir == Direction::DESC {
        o.reverse()
    } else {
        o
    }
}

/// A non-file row (`tracker_items`): field name → value.
pub type Record = Arc<std::collections::BTreeMap<String, Value>>;

/// Runs `view` over index files.
pub fn run(env: &Env<'_>, view: &View) -> R<ViewResult> {
    run_on(env, view, None)
}

/// Runs `view` over `records` when given (no SQL narrowing; filters evaluate per record),
/// otherwise over index files.
pub fn run_on(env: &Env<'_>, view: &View, records: Option<&[Record]>) -> R<ViewResult> {
    run_sorted(env, view, records, None)
}

/// [`run_on`] with the editor's column-header sort as the primary key of a table view
/// (an unknown column is ignored; pivot and tasks views ignore it).
pub fn run_sorted(
    env: &Env<'_>,
    view: &View,
    records: Option<&[Record]>,
    ui_sort: Option<&cv_protocol::ViewSort>,
) -> R<ViewResult> {
    run_selected(env, view, records, ui_sort, None)
}

/// [`run_sorted`] with the host's in-place selection (plan round-13 WP-4): rows whose
/// `select.property` evaluates to the text `select.value`, ANDed with the view's filters before
/// grouping, sorting, the limit and summaries.
pub fn run_selected(
    env: &Env<'_>,
    view: &View,
    records: Option<&[Record]>,
    ui_sort: Option<&cv_protocol::ViewSort>,
    select: Option<&cv_protocol::ViewSelect>,
) -> R<ViewResult> {
    let kind = match view.kind {
        ViewType::Table => Kind::Table,
        ViewType::Pivot => Kind::Pivot,
        ViewType::Tasks => Kind::Tasks,
    };
    let mut notices = view.errors.clone();
    if view.filter_error {
        let mut res = ViewResult::empty(kind, notices);
        res.max_height = view.max_height;
        res.select_by.clone_from(&view.select_by);
        return Ok(res);
    }
    let rows: Vec<(Option<Arc<FileInfo>>, RowCtx<'_>)> = match records {
        Some(recs) => recs
            .iter()
            .map(|r| (None, RowCtx::record(env, Arc::clone(r))))
            .collect(),
        None => {
            let unbound = RowCtx::new(env, None);
            let n = narrow(&view.filters, &unbound)?;
            let cands = env.db.candidates(&n.sql, &n.args)?;
            if cands.len() > PRELOAD_AT {
                env.db.preload(&cands)?;
            }
            cands
                .into_iter()
                .map(|f| (Some(Arc::clone(&f)), RowCtx::new(env, Some(f))))
                .collect()
        }
    };
    let selected = select.map(|s| (crate::expr::property(&s.property), s.value.as_str()));
    let mut kept: Vec<(Option<Arc<FileInfo>>, RowCtx<'_>)> = Vec::new();
    for (f, ctx) in rows {
        let mut ok = true;
        for flt in &view.filters {
            if !passes(&ctx, flt)? {
                ok = false;
                break;
            }
        }
        if ok && let Some((e, want)) = &selected {
            ok = matches!(ctx.eval(e)?, Value::Str(ref v) if v.as_str() == *want);
        }
        if ok {
            kept.push((f, ctx));
        }
    }
    let mut res = match (view.kind, &view.pivot, &view.tasks) {
        (ViewType::Pivot, Some(p), _) => pivot(env, p, &kept)?,
        (ViewType::Pivot, None, _) => ViewResult::empty(kind, Vec::new()),
        (ViewType::Tasks, _, Some(t)) => tasks(env, t, view.limit, &kept)?,
        _ => table(view, kept, ui_sort)?,
    };
    if records.is_some() && res.kind == Kind::Table {
        // A record source's `status` column is
        // the task-status cell (`tracker_items.status` → set-status in the editor).
        for c in &mut res.columns {
            if c.key == STATUS_COLUMN && c.role.is_none() {
                c.role = Some(ColumnRole::Status);
            }
        }
    }
    notices.extend(env.notices.borrow().iter().cloned());
    res.notices = notices;
    res.max_height = view.max_height;
    res.select_by.clone_from(&view.select_by);
    Ok(res)
}

fn table(
    view: &View,
    kept: Vec<(Option<Arc<FileInfo>>, RowCtx<'_>)>,
    ui_sort: Option<&cv_protocol::ViewSort>,
) -> R<ViewResult> {
    let ui = ui_sort.and_then(|s| {
        let dir = match s.direction {
            cv_protocol::ViewSortDir::Asc => Direction::ASC,
            cv_protocol::ViewSortDir::Desc => Direction::DESC,
        };
        view.order
            .iter()
            .position(|(id, _, _)| *id == s.property)
            .map(|i| (i, dir))
    });
    let mut recs = Vec::with_capacity(kept.len());
    for (file, ctx) in kept {
        let mut cells = Vec::with_capacity(view.order.len());
        for (_, e, _) in &view.order {
            cells.push(ctx.eval(e)?);
        }
        let mut sort = Vec::with_capacity(view.sort.len());
        for (e, _) in &view.sort {
            sort.push(ctx.eval(e)?);
        }
        let group = match &view.group {
            Some((e, _)) => ctx.eval(e)?,
            None => Value::Null,
        };
        let mut flags = Vec::with_capacity(view.summary_rows.len());
        for r in &view.summary_rows {
            flags.push(ctx.eval(&r.filter)?.truthy());
        }
        let key = ctx.record_key();
        let select = ctx.select_value();
        recs.push(Rec {
            file,
            key,
            select,
            cells,
            sort,
            group,
            flags,
        });
    }
    recs.sort_by(|a, b| {
        if let Some((c, dir)) = ui
            && let (Some(x), Some(y)) = (a.cells.get(c), b.cells.get(c))
        {
            let o = ordered(dir, sort_cmp(x, y));
            if o.is_ne() {
                return o;
            }
        }
        for (i, (_, dir)) in view.sort.iter().enumerate() {
            let o = match (a.sort.get(i), b.sort.get(i)) {
                (Some(x), Some(y)) => ordered(*dir, sort_cmp(x, y)),
                _ => std::cmp::Ordering::Equal,
            };
            if o.is_ne() {
                return o;
            }
        }
        match (&a.file, &b.file) {
            (Some(x), Some(y)) => x.path.cmp(&y.path),
            // Records keep their source order (the sort is stable).
            _ => std::cmp::Ordering::Equal,
        }
    });
    if let Some(l) = view.limit {
        recs.truncate(l);
    }
    if let Some((_, dir)) = &view.group {
        recs.sort_by(|a, b| ordered(*dir, sort_cmp(&a.group, &b.group)));
    }
    // (column index, fn) for summarized columns; summaries of undisplayed properties are
    // dropped, as in Bases (footers exist only under columns).
    let sums: Vec<(usize, SumFn)> = view
        .summaries
        .iter()
        .filter_map(|(k, f)| {
            view.order
                .iter()
                .position(|(id, _, _)| same_prop(id, k))
                .map(|i| (i, *f))
        })
        .collect();
    let mut fns: Vec<SumFn> = Vec::new();
    for (_, f) in &sums {
        if !fns.contains(f) {
            fns.push(*f);
        }
    }
    // Filtered rows: per summary row, the aggregate of each displayed column (plan round-10 WP-4).
    let row_fns: Vec<Vec<Option<SumFn>>> = view
        .summary_rows
        .iter()
        .map(|sr| {
            (0..view.order.len())
                .map(|c| {
                    sr.summaries
                        .iter()
                        .find_map(|(k, f)| same_prop(&view.order[c].0, k).then_some(*f))
                })
                .collect()
        })
        .collect();
    let footer = |rs: &[&Rec]| -> Vec<Summary> {
        let plain = fns
            .iter()
            .map(|f| Summary {
                label: f.label().to_string(),
                cells: (0..view.order.len())
                    .map(|c| {
                        if sums.iter().any(|(i, g)| *i == c && g == f) {
                            let vals: Vec<&Value> =
                                rs.iter().filter_map(|r| r.cells.get(c)).collect();
                            summarize(*f, &vals).to_cell()
                        } else {
                            Cell::Null
                        }
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        // An empty filter match yields null cells (never the plain footer's Sum/Count zeros).
        let filtered = view.summary_rows.iter().enumerate().map(|(i, sr)| {
            let matched: Vec<&Rec> = rs.iter().copied().filter(|r| r.flags[i]).collect();
            Summary {
                label: sr.label.clone(),
                cells: row_fns[i]
                    .iter()
                    .enumerate()
                    .map(|(c, f)| match f {
                        Some(f) if !matched.is_empty() => {
                            let vals: Vec<&Value> =
                                matched.iter().filter_map(|r| r.cells.get(c)).collect();
                            summarize(*f, &vals).to_cell()
                        }
                        _ => Cell::Null,
                    })
                    .collect(),
            }
        });
        plain.into_iter().chain(filtered).collect()
    };
    let to_row = |r: &Rec| Row {
        path: r.file.as_ref().map(|f| f.path.clone()),
        line: None,
        key: r.key.clone(),
        select: r.select.clone(),
        cells: r
            .cells
            .iter()
            .zip(&view.order)
            .map(|(v, (id, _, _))| match &r.file {
                Some(f) if id == "file.name" => Cell::Link {
                    path: f.path.clone(),
                    label: f.name.clone(),
                },
                _ => v.to_cell(),
            })
            .collect(),
    };
    let all: Vec<&Rec> = recs.iter().collect();
    let mut groups = Vec::new();
    if view.group.is_some() {
        let mut start = 0;
        while start < recs.len() {
            let mut end = start + 1;
            while end < recs.len() && sort_cmp(&recs[start].group, &recs[end].group).is_eq() {
                end += 1;
            }
            let slice: Vec<&Rec> = recs[start..end].iter().collect();
            groups.push(Group {
                key: Some(recs[start].group.to_cell()),
                rows: slice.iter().map(|r| to_row(r)).collect(),
                summaries: footer(&slice),
            });
            start = end;
        }
    } else {
        groups.push(Group {
            key: None,
            rows: all.iter().map(|r| to_row(r)).collect(),
            summaries: Vec::new(),
        });
    }
    Ok(ViewResult {
        kind: Kind::Table,
        max_height: None,
        select_by: None,
        columns: view
            .order
            .iter()
            .map(|(id, _, l)| {
                let mut col = Column::plain(id.clone(), l.clone());
                col.width = view.column_widths.get(id).copied();
                match view.checkbox_key(id) {
                    Some(key) => col.with_role(ColumnRole::Checkbox { key }),
                    None if id == "note.name" || id == "name" => col.with_role(ColumnRole::Name),
                    None => col,
                }
            })
            .collect(),
        groups,
        summaries: footer(&all),
        notices: Vec::new(),
    })
}

fn pivot(
    env: &Env<'_>,
    p: &crate::def::Pivot,
    kept: &[(Option<Arc<FileInfo>>, RowCtx<'_>)],
) -> R<ViewResult> {
    let ncol = p.columns.len();
    let mut keys: Vec<(Value, Vec<f64>, Vec<bool>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (_, ctx) in kept {
        let k = ctx.eval(&p.rows)?;
        let kt = k.to_text();
        if kt.is_empty() {
            continue;
        }
        let mut col = None;
        for (i, (_, when)) in p.columns.iter().enumerate() {
            let hit = match when {
                None => true,
                Some(w) => ctx.eval(w)?.truthy(),
            };
            if hit {
                col = Some(i);
                break;
            }
        }
        let Some(c) = col else { continue };
        let v = match p.aggregate {
            Aggregate::Sum => {
                let n = ctx.eval(&p.value)?.to_num();
                if n.is_finite() { n } else { 0.0 }
            }
            Aggregate::Count => 1.0,
        };
        let i = *index.entry(kt).or_insert_with(|| {
            keys.push((k, vec![0.0; ncol], vec![false; ncol]));
            keys.len() - 1
        });
        if let Some((_, sums, has)) = keys.get_mut(i) {
            sums[c] += v;
            has[c] = true;
        }
    }
    keys.sort_by(|a, b| ordered(p.rows_direction, sort_cmp(&a.0, &b.0)));
    let unbound = RowCtx::new(env, None);
    let disp = |n: f64| -> R<Cell> {
        match &p.display {
            Some(d) => Ok(unbound.eval_with(d, Value::Num(n))?.to_cell()),
            None => Ok(Value::Num(n).to_cell()),
        }
    };
    let mut columns = vec![Column::plain("rows", p.rows_label.clone())];
    columns.extend(
        p.columns
            .iter()
            .map(|(l, _)| Column::plain(l.clone(), l.clone())),
    );
    if let Some(t) = &p.total_column {
        columns.push(Column::plain("total", t.clone()));
    }
    let mut rows = Vec::with_capacity(keys.len());
    let mut col_tot = vec![0.0; ncol];
    for (k, sums, has) in &keys {
        let mut cells = vec![k.to_cell()];
        for c in 0..ncol {
            col_tot[c] += sums[c];
            cells.push(if has[c] { disp(sums[c])? } else { Cell::Null });
        }
        if p.total_column.is_some() {
            cells.push(disp(sums.iter().fold(0.0, |a, n| a + n))?);
        }
        rows.push(Row {
            path: None,
            line: None,
            key: None,
            select: None,
            cells,
        });
    }
    let grand: f64 = col_tot.iter().fold(0.0, |a, n| a + n);
    let mut summaries = Vec::new();
    let summary_row = |label: &str, div: f64| -> R<Summary> {
        let mut cells = vec![Cell::Text { v: label.into() }];
        for t in &col_tot {
            cells.push(if div > 0.0 {
                disp(t / div)?
            } else {
                Cell::Null
            });
        }
        if p.total_column.is_some() {
            cells.push(if div > 0.0 {
                disp(grand / div)?
            } else {
                Cell::Null
            });
        }
        Ok(Summary {
            label: label.into(),
            cells,
        })
    };
    if let Some(l) = &p.total_row {
        summaries.push(summary_row(l, 1.0)?);
    }
    if let Some(l) = &p.average_row {
        #[allow(clippy::cast_precision_loss)]
        summaries.push(summary_row(l, keys.len() as f64)?);
    }
    Ok(ViewResult {
        kind: Kind::Pivot,
        max_height: None,
        select_by: None,
        columns,
        groups: vec![Group {
            key: None,
            rows,
            summaries: Vec::new(),
        }],
        summaries,
        notices: Vec::new(),
    })
}

fn tasks(
    env: &Env<'_>,
    t: &crate::def::Tasks,
    limit: Option<usize>,
    kept: &[(Option<Arc<FileInfo>>, RowCtx<'_>)],
) -> R<ViewResult> {
    let mut groups: Vec<(String, Cell, Vec<Row>)> = Vec::new();
    let mut total = 0usize;
    'files: for (f, ctx) in kept {
        let Some(f) = f else { continue };
        let file_tasks = env.db.tasks(f)?;
        for task in file_tasks.iter() {
            if !t.status.is_empty() && !t.status.contains(&task.status) {
                continue;
            }
            if t.require_text && task.text.trim().is_empty() {
                continue;
            }
            if let Some(flt) = &t.task_filter
                && !ctx.eval_with(flt, task_value(task))?.truthy()
            {
                continue;
            }
            if limit.is_some_and(|l| total >= l) {
                break 'files;
            }
            total += 1;
            let (key, cell) = match t.group {
                TaskGroup::Heading => {
                    let h = task.heading.clone().unwrap_or_else(|| "General".into());
                    (h.clone(), Cell::Text { v: h })
                }
                TaskGroup::File => (
                    f.path.clone(),
                    Cell::Link {
                        path: f.path.clone(),
                        label: f.name.clone(),
                    },
                ),
                TaskGroup::None => (String::new(), Cell::Null),
            };
            let row = Row {
                path: Some(f.path.clone()),
                line: Some(task.line),
                key: None,
                select: None,
                cells: vec![
                    Cell::Text {
                        v: task.text.clone(),
                    },
                    Cell::Link {
                        path: f.path.clone(),
                        label: f.name.clone(),
                    },
                    // Plan round-6-feedback-todos WP-6: the task's status char (` ` = open).
                    Cell::Text {
                        v: task.status.clone(),
                    },
                ],
            };
            match groups.iter_mut().find(|g| g.0 == key) {
                Some(g) => g.2.push(row),
                None => groups.push((key, cell, vec![row])),
            }
        }
    }
    Ok(ViewResult {
        kind: Kind::Tasks,
        max_height: None,
        select_by: None,
        columns: vec![
            Column::plain("task", "Task").with_role(ColumnRole::TaskText),
            Column::plain("file", "File").with_role(ColumnRole::TaskFile),
            Column::plain("status", "Status").with_role(ColumnRole::Status),
        ],
        groups: groups
            .into_iter()
            .map(|(_, cell, rows)| Group {
                key: (t.group != TaskGroup::None).then_some(cell),
                rows,
                summaries: Vec::new(),
            })
            .collect(),
        summaries: Vec::new(),
        notices: Vec::new(),
    })
}
