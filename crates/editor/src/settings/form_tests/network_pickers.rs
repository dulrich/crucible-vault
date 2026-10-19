//! Network-backed pickers (currency, weather, models, channels, fx).

use super::*;

const LOCAL_PROVIDER: &str = "[[provider]]\nid = \"local\"\nkind = \"lmstudio\"\nname = \"Local\"\n\n[[provider.models]]\nid = \"gemma-4\"\n\n[[provider.models]]\nid = \"qwen-old\"\n";

#[test]
fn currency_picker_shows_code_and_name_and_stores_the_code() {
    use crate::widget::typeahead::Key;
    let (_t, d) = fake();
    let mut app = app_over(&d);
    put(
        &mut app.settings,
        PIPES,
        "[[daily_brief.fx]]\nbase = \"USD\"\nquote = \"EUR\"\n",
    );
    type_in(&mut app, PIPES, "daily_brief.fx[0].quote", "mex");
    let vis = app.settings.ta_visible(&[]);
    assert_eq!(vis[0].label, "MXN — Mexican Peso");
    assert_eq!(vis[0].value, "MXN");
    let _ = app.send(s(SettingsMsg::Typeahead(Key::Enter)));
    assert!(doc_text(&app.settings, PIPES).contains("quote = \"MXN\""));
    assert_eq!(
        pickers::label_of(&app.settings, form::Picker::Currency, "USD").as_deref(),
        Some("USD — United States Dollar")
    );
}

#[test]
fn weather_pick_writes_label_lat_lon_in_one_edit_and_short_queries_never_geocode() {
    let (_t, d) = fake();
    let mut app = app_over(&d);
    put(
        &mut app.settings,
        PIPES,
        "[[daily_brief.weather]]\nlabel = \"Home\"\nlat = 1.0\nlon = 2.0\n",
    );
    // One character: no lookup is wanted, nothing cached.
    type_in(&mut app, PIPES, "daily_brief.weather[0].label", "p");
    assert!(app.settings.lookups.geo.is_empty());
    type_in(&mut app, PIPES, "daily_brief.weather[0].label", "par");
    assert_eq!(app.settings.lookups.geo.len(), 1);
    let vis = app.settings.ta_visible(&[]);
    assert_eq!(vis.len(), 1);
    assert_eq!(
        (vis[0].label.as_str(), vis[0].secondary.as_str()),
        ("Paris, FR", "Ile-de-France")
    );
    let before = app.settings.file(PIPES).expect("f").edit_seq();
    let _ = app.send(s(SettingsMsg::Form(
        PIPES.to_owned(),
        FormMsg::Pick {
            path: p("daily_brief.weather[0].label"),
            value: vis[0].value.clone(),
        },
    )));
    let text = doc_text(&app.settings, PIPES);
    assert!(text.contains("label = \"Paris, FR\""), "{text}");
    assert!(text.contains("lat = 48.853"), "{text}");
    assert!(text.contains("lon = 2.349"), "{text}");
    assert_eq!(app.settings.file(PIPES).expect("f").edit_seq(), before + 1);
}

#[test]
fn model_picker_merges_live_and_declared_models() {
    let (_t, d) = fake();
    let mut app = app_over(&d);
    put(&mut app.settings, PROVIDERS, LOCAL_PROVIDER);
    put(
        &mut app.settings,
        PIPES,
        "[image_describe]\nprovider = \"local\"\nmodel = \"\"\n",
    );
    type_in(&mut app, PIPES, "image_describe.model", "");
    let all = pickers::merged_models(&app.settings, Some("local"));
    let got: Vec<(&str, &str)> = all
        .iter()
        .map(|c| (c.value.as_str(), c.secondary.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("gemma-4", "live"),
            ("bge-m3", "live"),
            ("qwen-old", "declared")
        ]
    );
    assert_eq!(
        app.settings.ta_status(),
        crate::widget::typeahead::Status::Ready
    );
}

#[test]
fn model_rpc_failure_falls_back_to_declared_with_a_status_line() {
    let (_t, d) = fake();
    d.set_lookups(crate::daemon::FakeLookups {
        fail: Some("upstream down".into()),
        ..Default::default()
    })
    .expect("lookups");
    let mut app = app_over(&d);
    put(&mut app.settings, PROVIDERS, LOCAL_PROVIDER);
    put(
        &mut app.settings,
        PIPES,
        "[image_describe]\nprovider = \"local\"\nmodel = \"\"\n",
    );
    type_in(&mut app, PIPES, "image_describe.model", "");
    let vis = app.settings.ta_visible(&[]);
    let vals: Vec<&str> = vis.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(vals, vec!["gemma-4", "qwen-old"]);
    assert!(vis.iter().all(|c| c.secondary == "declared"));
    assert_eq!(
        app.settings.ta_status(),
        crate::widget::typeahead::Status::Failed(lookups::MODELS_FAILED.to_owned())
    );
}

#[test]
fn provider_model_reference_splits_and_writes_back_the_prefix() {
    use crate::widget::typeahead::Key;
    let (_t, d) = fake();
    let mut app = app_over(&d);
    put(&mut app.settings, PROVIDERS, LOCAL_PROVIDER);
    put(
        &mut app.settings,
        AGENTS,
        "[writer]\nmodel = \"local:qwen-old\"\nmax_tokens = 10\n",
    );
    assert_eq!(
        picker_of(AGENTS, &p("writer.model")),
        Some(form::Picker::ModelRef)
    );
    type_in(&mut app, AGENTS, "writer.model", "local:bge");
    let vis = app.settings.ta_visible(&[]);
    assert_eq!(vis[0].label, "bge-m3");
    assert_eq!(vis[0].value, "local:bge-m3");
    let _ = app.send(s(SettingsMsg::Typeahead(Key::Enter)));
    assert!(doc_text(&app.settings, AGENTS).contains("model = \"local:bge-m3\""));
    // `{param.…}` stays free text: no candidates, valid as typed.
    type_in(&mut app, AGENTS, "writer.model", "{param.m}");
    assert!(app.settings.ta_visible(&[]).is_empty());
    let _ = app.send(s(SettingsMsg::Typeahead(Key::Enter)));
    assert!(doc_text(&app.settings, AGENTS).contains("model = \"{param.m}\""));
}

#[test]
fn unknown_channel_ids_show_fetched_titles() {
    let (_t, d) = fake();
    let mut app = app_over(&d);
    put(
        &mut app.settings,
        SOURCES,
        "[[youtube]]\nchannel = \"Chan\"\nid = \"UC123\"\n\n[[youtube]]\nchannel = \"\"\nid = \"UCfake\"\n",
    );
    put(
        &mut app.settings,
        PIPES,
        "[ignore_shorts]\nchannels = [\"UC123\", \"UCfake\", \"UCnope\"]\n",
    );
    let _ = app.send(s(SettingsMsg::Section(Section::Pipelines)));
    let st = &app.settings;
    let chans = pickers::channels(st);
    assert_eq!(pickers::chip_label(st, &chans, "UC123"), "Chan (UC123)");
    assert_eq!(
        pickers::chip_label(st, &chans, "UCfake"),
        "Fake Channel (UCfake)"
    );
    assert_eq!(pickers::chip_label(st, &chans, "UCnope"), "UCnope");
    assert_eq!(
        pickers::source_channel_placeholder(st, SOURCES, &p("youtube[1].channel")).as_deref(),
        Some("Fake Channel")
    );
    // Never auto-written.
    assert!(doc_text(st, SOURCES).contains("channel = \"\""));
}

/// Plan round-6-feedback-todos WP-8: an `fx` item with no label summarises as `BASE → QUOTE`.
#[test]
fn fx_rows_summarise_as_base_to_quote_without_a_label() {
    let st = loaded(
        PIPES,
        "[[daily_brief.fx]]\nbase = \"USD\"\nquote = \"MXN\"\n\n[[daily_brief.fx]]\nbase = \"USD\"\nquote = \"EUR\"\nlabel = \"Euro\"\n",
    );
    let sums: Vec<String> = rows_of(&st, PIPES)
        .iter()
        .filter(|r| r.path.to_string().starts_with("daily_brief.fx["))
        .filter_map(|r| match &r.kind {
            Kind::Item { summary, .. } => Some(summary.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(sums, vec!["USD \u{2192} MXN", "Euro"]);
}

/// Plan round-6-feedback-todos WP-8: a failed model lookup is re-fetched when the field is
/// re-opened (not on every keystroke while open) and on the refresh control; models show their
/// label with the id beneath, and still filter by id.
#[test]
fn failed_model_lookup_refetches_on_reopen_and_refresh() {
    use crate::widget::typeahead::Key;
    let (_t, d) = fake();
    d.set_lookups(crate::daemon::FakeLookups {
        fail: Some("upstream down".into()),
        ..Default::default()
    })
    .expect("lookups");
    let mut app = app_over(&d);
    put(
        &mut app.settings,
        PROVIDERS,
        "[[provider]]\nid = \"local\"\nkind = \"lmstudio\"\n\n[[provider.models]]\nid = \"qwen-old\"\nlabel = \"Qwen (old)\"\n",
    );
    put(
        &mut app.settings,
        PIPES,
        "[image_describe]\nprovider = \"local\"\nmodel = \"\"\n",
    );
    let failed = || Some(&lookups::Models::Failed);
    type_in(&mut app, PIPES, "image_describe.model", "");
    assert_eq!(app.settings.lookups.models.get("local"), failed());
    let vis = app.settings.ta_visible(&[]);
    assert_eq!(
        (
            vis[0].label.as_str(),
            vis[0].secondary.as_str(),
            vis[0].value.as_str()
        ),
        ("Qwen (old)", "qwen-old · declared", "qwen-old")
    );
    // The daemon recovers; keystrokes while the list is open do not refetch.
    d.set_lookups(crate::daemon::FakeLookups::default())
        .expect("lookups");
    type_in(&mut app, PIPES, "image_describe.model", "qw");
    assert_eq!(app.settings.lookups.models.get("local"), failed());
    // Filtering still matches the id behind a label.
    assert!(
        app.settings
            .ta_visible(&[])
            .iter()
            .any(|c| c.value == "qwen-old")
    );
    // Close and re-open: refetched.
    let _ = app.send(s(SettingsMsg::Typeahead(Key::Escape)));
    type_in(&mut app, PIPES, "image_describe.model", "");
    assert!(matches!(
        app.settings.lookups.models.get("local"),
        Some(lookups::Fetch::Ok(_))
    ));
    // The refresh control refetches too (here: back to failed).
    d.set_lookups(crate::daemon::FakeLookups {
        fail: Some("down again".into()),
        ..Default::default()
    })
    .expect("lookups");
    let _ = app.send(s(SettingsMsg::Form(
        PIPES.to_owned(),
        FormMsg::RefreshModels {
            path: p("image_describe.model"),
            picker: form::Picker::Model,
        },
    )));
    assert_eq!(app.settings.lookups.models.get("local"), failed());
}
