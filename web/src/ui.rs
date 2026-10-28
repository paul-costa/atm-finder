//! The page logic (browser only).
//!
//! The page skeleton lives in `static/index.html`; this module fills it with translated text,
//! reacts to the user and renders the dynamic parts: the list of boundary candidates, the result
//! card, the ATM list and the ready-made files. The map, the file download and the clipboard
//! are done in JavaScript (`static/map.js`), because Leaflet is a JavaScript library.
//!
//! The chosen place is kept in the page address (`?place=Graz`, plus `&level=district` when the
//! name alone is ambiguous), so a result can be bookmarked and shared; opening such an address
//! runs the search right away. The level is always picked automatically; `level` from the
//! address only narrows the lookup.
//!
//! Public Google Maps lists come from the server next to the site (`api/lists`): a link with the
//! Google Maps logo for every place that has one, and a button that asks the server to create
//! the list for a new place. Only one list is created at a time, followed by a cooldown.
//!
//! State lives in one `App` behind `Rc<RefCell<..>>`: event handlers and async tasks each hold a
//! clone of the `Rc`. Borrows are kept short and never held across an `.await`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gloo_timers::future::sleep;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Document, Element, Event, HtmlElement, HtmlInputElement, UrlSearchParams};

use atm_finder_core::boundary::{self, Boundary, LOOKUP_LEVELS};
use atm_finder_core::export;
use atm_finder_core::model::{merge, Atm};
use atm_finder_core::places::slugify;
use atm_finder_core::MERGE_RADIUS_M;

use crate::engine::{self, FillCounts, ListsState, ReadyEntry};
use crate::i18n::{fill, Lang};

/// Google Maps logo next to the list links (`static/gmaps.svg`).
const GMAPS_LOGO: &str = "gmaps.svg";
/// How often the lists are reloaded while one is being created.
const LIST_POLL: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- JavaScript side (static/map.js)

#[wasm_bindgen]
extern "C" {
    /// `window.atmMap.init(id)`: create the Leaflet map in the element with this id.
    #[wasm_bindgen(js_namespace = atmMap, js_name = init)]
    fn map_init(id: &str);
    /// `window.atmMap.show(atms, boundary, labels)`: draw the ATMs and the boundary, all as JSON text.
    #[wasm_bindgen(js_namespace = atmMap, js_name = show)]
    fn map_show(atms_geojson: &str, boundary_geojson: &str, labels: &str);
    /// `window.atmMap.clear()`: remove everything from the map.
    #[wasm_bindgen(js_namespace = atmMap, js_name = clear)]
    fn map_clear();
    /// `window.atmDownload(name, mime, text)`: offer a text as a file download.
    #[wasm_bindgen(js_name = atmDownload)]
    fn js_download(name: &str, mime: &str, text: &str);
    /// `window.atmCopy(text)`: put a text on the clipboard; the promise settles when done.
    #[wasm_bindgen(js_name = atmCopy)]
    fn js_copy(text: &str) -> js_sys::Promise;
}

// ---------------------------------------------------------------- state

/// Everything the page knows at any moment.
struct App {
    lang: Lang,
    /// Boundaries the last search found, shown as buttons until one is chosen.
    candidates: Vec<Boundary>,
    /// The level from the page address the search is limited to (`district`, ...), else empty.
    search_level: String,
    /// The chosen boundary; `None` until a search has succeeded.
    boundary: Option<Boundary>,
    /// The merged ATMs of that boundary.
    atms: Vec<Atm>,
    /// Number of raw records the ATMs were merged from, for the summary line.
    raw_count: usize,
    /// A request is in flight: buttons are disabled and new searches ignored.
    busy: bool,
    /// Set once addresses have been completed for the current result.
    fill_counts: Option<FillCounts>,
    /// The ready-made exports from `data/index.json`.
    ready: Vec<ReadyEntry>,
    /// Google Maps lists and the creation lock; `None` when the server API is not reachable.
    lists: Option<ListsState>,
    /// A poll loop for the lists is running.
    polling: bool,
}

type Shared = Rc<RefCell<App>>;

// ---------------------------------------------------------------- DOM helpers

fn document() -> Document {
    web_sys::window()
        .expect("window")
        .document()
        .expect("document")
}

/// The element with this id; every id used here exists in `index.html`, so a miss is a bug.
fn by_id(id: &str) -> Element {
    document()
        .get_element_by_id(id)
        .unwrap_or_else(|| panic!("missing #{id}"))
}

fn set_text(id: &str, text: &str) {
    by_id(id).set_text_content(Some(text));
}

/// Show or hide an element through the `hidden` attribute.
fn show(id: &str, visible: bool) {
    let element = by_id(id);
    if visible {
        element.remove_attribute("hidden").ok();
    } else {
        element.set_attribute("hidden", "").ok();
    }
}

fn set_disabled(id: &str, disabled: bool) {
    let element = by_id(id);
    if disabled {
        element.set_attribute("disabled", "").ok();
    } else {
        element.remove_attribute("disabled").ok();
    }
}

/// A new element with optional class and text content.
fn create(tag: &str, class: &str, text: &str) -> Element {
    let element = document().create_element(tag).expect("element");
    if !class.is_empty() {
        element.set_class_name(class);
    }
    if !text.is_empty() {
        element.set_text_content(Some(text));
    }
    element
}

/// A link that opens in a new tab.
fn link(href: &str, text: &str) -> Element {
    let anchor = create("a", "", text);
    anchor.set_attribute("href", href).ok();
    anchor.set_attribute("target", "_blank").ok();
    anchor.set_attribute("rel", "noopener").ok();
    anchor
}

/// Attach an event handler for the lifetime of the page (the closure is leaked on purpose).
fn on(element: &Element, event: &str, handler: impl FnMut(Event) + 'static) {
    let closure = Closure::<dyn FnMut(Event)>::new(handler);
    element
        .add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())
        .expect("listener");
    closure.forget();
}

fn clear_children(element: &Element) {
    while let Some(child) = element.first_child() {
        element.remove_child(&child).ok();
    }
}

fn storage_get(key: &str) -> Option<String> {
    let storage = web_sys::window()?.local_storage().ok()??;
    storage.get_item(key).ok()?
}

fn storage_set(key: &str, value: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    if let Ok(Some(storage)) = window.local_storage() {
        storage.set_item(key, value).ok();
    }
}

/// `place` and `level` from the page address (`?place=Graz&level=district`), when a place is given.
fn place_from_url() -> Option<(String, String)> {
    let search = web_sys::window()?.location().search().ok()?;
    let params = UrlSearchParams::new_with_str(&search).ok()?;
    let place = params.get("place")?.trim().to_string();
    if place.is_empty() {
        return None;
    }
    Some((place, params.get("level").unwrap_or_default()))
}

/// The relative page address that opens a place: `?place=Graz`, with `&level=...` when given.
fn place_link(place: &str, level: &str) -> String {
    let Ok(params) = UrlSearchParams::new() else {
        return String::new();
    };
    params.append("place", place);
    if !level.is_empty() {
        params.append("level", level);
    }
    format!("?{}", String::from(params.to_string()))
}

/// Replace the page address without reloading (no new history entry).
fn set_page_link(link: &str) {
    if let Some(history) = web_sys::window().and_then(|window| window.history().ok()) {
        history
            .replace_state_with_url(&JsValue::NULL, "", Some(link))
            .ok();
    }
}

/// A Google Maps search for the position; "Save" there adds the place to a list.
fn gmaps_url(atm: &Atm) -> String {
    format!(
        "https://www.google.com/maps/search/?api=1&query={:.6}%2C{:.6}",
        atm.lat, atm.lon
    )
}

/// Operator, brand or name, whichever is known first.
fn atm_label(atm: &Atm) -> &str {
    [&atm.operator, &atm.brand, &atm.name]
        .into_iter()
        .find(|value| !value.is_empty())
        .map(String::as_str)
        .unwrap_or("ATM")
}

// ---------------------------------------------------------------- rendering

/// The status line under the search form; an empty text hides it.
fn set_status(text: &str, is_error: bool) {
    let element = by_id("status");
    element.set_text_content(Some(text));
    element.set_class_name(if is_error { "status error" } else { "status" });
    show("status", !text.is_empty());
}

/// Kind and state of a boundary in the page language, e.g. `Stadt · Steiermark`.
/// A state does not repeat its own name.
fn level_and_state(boundary: &Boundary, lang: Lang) -> String {
    let mut text = lang.t(boundary.kind_key()).to_string();
    let state = boundary.state();
    if !state.is_empty() && boundary.admin_level != 4 {
        text = format!("{text} · {state}");
    }
    text
}

/// Set every `data-i18n` text and `data-i18n-placeholder` on the page, then re-render the
/// dynamic parts, which contain translated text too.
fn apply_i18n(app: &Shared) {
    let lang = app.borrow().lang;
    let document = document();
    if let Some(html) = document.document_element() {
        html.set_attribute("lang", lang.code()).ok();
    }
    document.set_title(lang.t("title"));

    translate_attribute_targets(
        &document,
        "data-i18n",
        |element, text| element.set_text_content(Some(text)),
        lang,
    );
    translate_attribute_targets(
        &document,
        "data-i18n-placeholder",
        |element, text| {
            element.set_attribute("placeholder", text).ok();
        },
        lang,
    );

    render_candidates(app);
    render_results(app);
    render_ready(app);
    render_gmaps(app);
}

/// Apply `set` with the translation of the key stored in `attribute` to every element that has it.
fn translate_attribute_targets(
    document: &Document,
    attribute: &str,
    set: impl Fn(&Element, &str),
    lang: Lang,
) {
    let Ok(nodes) = document.query_selector_all(&format!("[{attribute}]")) else {
        return;
    };
    for index in 0..nodes.length() {
        let Some(element) = nodes
            .item(index)
            .and_then(|node| node.dyn_into::<Element>().ok())
        else {
            continue;
        };
        if let Some(key) = element.get_attribute(attribute) {
            set(&element, lang.t(&key));
        }
    }
}

/// The list of boundaries to choose from; hidden when there are none.
fn render_candidates(app: &Shared) {
    let list = by_id("candidate-list");
    clear_children(&list);
    let (candidates, lang) = {
        let state = app.borrow();
        (state.candidates.clone(), state.lang)
    };
    show("candidates", !candidates.is_empty());
    for (index, candidate) in candidates.iter().enumerate() {
        let button = create("button", "candidate", "");
        button.set_attribute("type", "button").ok();
        button
            .append_child(&create("span", "candidate-name", &candidate.name))
            .ok();
        button
            .append_child(&create(
                "span",
                "candidate-meta",
                &level_and_state(candidate, lang),
            ))
            .ok();
        let app = app.clone();
        on(&button, "click", move |_| {
            select_candidate(&app, index, false)
        });
        list.append_child(&button).ok();
    }
}

/// The result card, download buttons and ATM list; hidden until a boundary is chosen.
fn render_results(app: &Shared) {
    let state = app.borrow();
    let lang = state.lang;
    let Some(boundary) = &state.boundary else {
        show("results", false);
        return;
    };
    show("results", true);

    let count = state.atms.len();
    let summary = if count == 0 {
        lang.t("result_none").to_string()
    } else {
        fill(
            lang.t("result_summary"),
            &[
                ("n", &count.to_string()),
                ("place", &boundary.name),
                ("raw", &state.raw_count.to_string()),
            ],
        )
    };
    set_text("result-summary", &summary);
    set_text(
        "result-place",
        &format!("{} · {}", boundary.name, level_and_state(boundary, lang)),
    );
    by_id("result-osm-link")
        .set_attribute("href", &boundary.url())
        .ok();

    // Address note: the outcome of a completed run, else how many addresses are missing.
    let missing = state
        .atms
        .iter()
        .filter(|atm| atm.street.is_empty())
        .count();
    let note = match state.fill_counts {
        Some(counts) => fill(
            lang.t("fill_done"),
            &[
                ("nearby", &counts.nearby.to_string()),
                ("reverse", &counts.reverse.to_string()),
                ("open", &counts.unresolved.to_string()),
            ],
        ),
        None if missing > 0 => fill(lang.t("addresses_missing"), &[("n", &missing.to_string())]),
        None => String::new(),
    };
    set_text("address-note", &note);
    show("address-note", !note.is_empty());
    let can_fill = state.fill_counts.is_none() && missing > 0 && count > 0;
    show("fill-button", can_fill);
    set_disabled("fill-button", state.busy);

    show("downloads", count > 0);
    show("list", count > 0);
    set_text("list-count", &count.to_string());

    let items = by_id("atm-items");
    clear_children(&items);
    for atm in &state.atms {
        let item = create("li", "atm", "");
        item.append_child(&create("strong", "", atm_label(atm)))
            .ok();
        let address = if atm.street.is_empty() {
            lang.t("no_address").to_string()
        } else {
            atm.address_line()
        };
        item.append_child(&create("span", "atm-address", &address))
            .ok();
        let links = create("span", "atm-links", "");
        links.append_child(&link(&atm.url, lang.t("open_osm"))).ok();
        links
            .append_child(&link(&gmaps_url(atm), lang.t("open_gmaps")))
            .ok();
        item.append_child(&links).ok();
        items.append_child(&item).ok();
    }
}

/// The ready-made files from `data/`; hidden when the index could not be loaded. The place
/// name is a link that shows the place on the map.
fn render_ready(app: &Shared) {
    let state = app.borrow();
    let lang = state.lang;
    let ready_list = by_id("ready-list");
    clear_children(&ready_list);
    show("ready", !state.ready.is_empty());
    for entry in &state.ready {
        let item = create("li", "ready", "");
        let title = create("strong", "", "");
        let name_link = create("a", "", &entry.name);
        name_link
            .set_attribute("href", &place_link(&entry.name, &entry.level))
            .ok();
        name_link.set_attribute("title", lang.t("ready_show")).ok();
        let (handler_app, name, level) = (app.clone(), entry.name.clone(), entry.level.clone());
        on(&name_link, "click", move |event| {
            event.prevent_default();
            if let Some(window) = web_sys::window() {
                window.scroll_to_with_x_and_y(0.0, 0.0);
            }
            start_search(&handler_app, &name, &level);
        });
        title.append_child(&name_link).ok();
        if !entry.level.is_empty() {
            let level_text = format!(" · {}", lang.t(&entry.level));
            title.append_child(&create("span", "", &level_text)).ok();
        }
        item.append_child(&title).ok();
        let mut meta = fill(lang.t("ready_rows"), &[("n", &entry.rows.to_string())]);
        if !entry.updated.is_empty() {
            meta = format!("{meta} · {}", entry.updated);
        }
        item.append_child(&create("span", "ready-meta", &meta)).ok();
        let links = create("span", "atm-links", "");
        for extension in ["csv", "geojson", "kml"] {
            let file_name = format!("{}.{extension}", entry.slug);
            let download = create("a", "", &extension.to_uppercase());
            download
                .set_attribute("href", &format!("data/{file_name}"))
                .ok();
            download.set_attribute("download", &file_name).ok();
            links.append_child(&download).ok();
        }
        let list = state
            .lists
            .as_ref()
            .and_then(|lists| lists.lists.iter().find(|list| list.slug == entry.slug));
        if let Some(list) = list {
            links.append_child(&gmaps_link(&list.url, lang)).ok();
        }
        item.append_child(&links).ok();
        ready_list.append_child(&item).ok();
    }
}

/// A link to a Google Maps list: the logo plus "Google Maps".
fn gmaps_link(url: &str, lang: Lang) -> Element {
    let anchor = link(url, "");
    anchor.set_class_name("gmaps-link");
    anchor.set_attribute("title", lang.t("gmaps_open")).ok();
    let logo = create("img", "gmaps-logo", "");
    logo.set_attribute("src", GMAPS_LOGO).ok();
    logo.set_attribute("alt", "").ok();
    anchor.append_child(&logo).ok();
    anchor.append_child(&create("span", "", "Google Maps")).ok();
    anchor
}

/// The Google Maps row of the result card: the list link when the place has one, else the
/// "create" button, disabled while the lock is held, with a note on why and until when.
fn render_gmaps(app: &Shared) {
    let state = app.borrow();
    let lang = state.lang;
    let (Some(boundary), Some(lists)) = (&state.boundary, &state.lists) else {
        show("gmaps-row", false);
        return;
    };
    if state.raw_count == 0 {
        show("gmaps-row", false);
        return;
    }
    show("gmaps-row", true);
    let existing = lists
        .lists
        .iter()
        .find(|list| list.relation == boundary.relation_id);
    let link_element = by_id("gmaps-list-link");
    clear_children(&link_element);
    let lock = &lists.lock;
    let note = if let Some(list) = existing {
        show("gmaps-create", false);
        link_element.set_attribute("href", &list.url).ok();
        link_element
            .set_attribute("title", lang.t("gmaps_open"))
            .ok();
        let logo = create("img", "gmaps-logo", "");
        logo.set_attribute("src", GMAPS_LOGO).ok();
        logo.set_attribute("alt", "").ok();
        link_element.append_child(&logo).ok();
        link_element
            .append_child(&create("span", "", lang.t("gmaps_open")))
            .ok();
        show("gmaps-list-link", true);
        if list.in_list < list.total {
            fill(
                lang.t("gmaps_partial"),
                &[
                    ("n", &list.in_list.to_string()),
                    ("total", &list.total.to_string()),
                ],
            )
        } else {
            String::new()
        }
    } else {
        show("gmaps-list-link", false);
        show("gmaps-create", true);
        let ours = lock.relation == boundary.relation_id;
        set_disabled("gmaps-create", state.busy || lock.state != "free");
        match lock.state.as_str() {
            "running" if ours => lang.t("gmaps_running_this").to_string(),
            "running" => fill(lang.t("gmaps_running_other"), &[("place", &lock.place)]),
            "cooldown" => fill(lang.t("gmaps_cooldown"), &[("time", &lock.until)]),
            _ if ours && !lock.error.is_empty() => {
                fill(lang.t("gmaps_failed"), &[("msg", &lock.error)])
            }
            _ => String::new(),
        }
    };
    set_text("gmaps-note", &note);
    show("gmaps-note", !note.is_empty());
}

/// Take a new lists state, re-render, and keep polling while a list is being created.
fn set_lists(app: &Shared, lists: ListsState) {
    let running = lists.lock.state == "running";
    app.borrow_mut().lists = Some(lists);
    render_ready(app);
    render_gmaps(app);
    if running && !app.borrow().polling {
        app.borrow_mut().polling = true;
        let app = app.clone();
        spawn_local(async move {
            loop {
                sleep(LIST_POLL).await;
                let Ok(lists) = engine::load_lists().await else {
                    continue;
                };
                let still_running = lists.lock.state == "running";
                app.borrow_mut().lists = Some(lists);
                render_ready(&app);
                render_gmaps(&app);
                if !still_running {
                    break;
                }
            }
            app.borrow_mut().polling = false;
        });
    }
}

/// "Create Google Maps list" was clicked.
fn on_create_list(app: &Shared) {
    let Some((relation, name)) = app
        .borrow()
        .boundary
        .as_ref()
        .map(|boundary| (boundary.relation_id, boundary.name.clone()))
    else {
        return;
    };
    set_disabled("gmaps-create", true);
    let app = app.clone();
    spawn_local(async move {
        match engine::create_list(relation, &name).await {
            Ok(lists) => set_lists(&app, lists),
            Err(message) => {
                let lang = app.borrow().lang;
                set_text(
                    "gmaps-note",
                    &fill(lang.t("gmaps_failed"), &[("msg", &message)]),
                );
                show("gmaps-note", true);
                set_disabled("gmaps-create", false);
            }
        }
    });
}

/// Hand the current ATMs and boundary to the map, with the popup labels in the page language.
fn update_map(app: &Shared) {
    let state = app.borrow();
    let Some(boundary) = &state.boundary else {
        map_clear();
        return;
    };
    let atms_geojson = export::to_geojson(&state.atms, None);
    let boundary_geojson = serde_json::to_string(&boundary.to_feature()).unwrap_or_default();
    let labels = serde_json::json!({
        "hours": state.lang.t("map_popup_hours"),
        "osm": state.lang.t("open_osm"),
        "gmaps": state.lang.t("open_gmaps"),
        "boundary": state.lang.t("map_boundary"),
        "no_address": state.lang.t("no_address"),
    });
    map_show(&atms_geojson, &boundary_geojson, &labels.to_string());
}

fn set_busy(app: &Shared, busy: bool) {
    app.borrow_mut().busy = busy;
    set_disabled("search-button", busy);
    set_disabled("fill-button", busy);
    render_gmaps(app);
}

// ---------------------------------------------------------------- actions

/// Fill the search form with a place and search, as if the user had typed it, limited to a
/// level key when one is given (empty or unknown = automatic). Used for `?place=` addresses and
/// the ready-made list.
fn start_search(app: &Shared, place: &str, level: &str) {
    let input = by_id("place-input")
        .dyn_into::<HtmlInputElement>()
        .expect("input");
    input.set_value(place);
    run_search(app, level);
}

/// The search form was submitted: look up the boundaries matching the typed place, on every level.
fn on_search(app: &Shared) {
    run_search(app, "");
}

/// Look up the boundaries matching the typed place, on one level when `level` is a level key.
fn run_search(app: &Shared, level: &str) {
    if app.borrow().busy {
        return;
    }
    let input = by_id("place-input")
        .dyn_into::<HtmlInputElement>()
        .expect("input");
    let query = input.value().trim().to_string();
    if query.is_empty() {
        return;
    }
    let level = level.to_string();
    let levels: Vec<u8> = match boundary::level_for(&level) {
        Some(level) => vec![level],
        None => LOOKUP_LEVELS.to_vec(),
    };

    // A new search clears the previous result.
    {
        let mut state = app.borrow_mut();
        state.candidates.clear();
        state.boundary = None;
        state.atms.clear();
        state.fill_counts = None;
        state.search_level = level.clone();
    }
    render_candidates(app);
    render_results(app);
    map_clear();
    set_busy(app, true);
    let lang = app.borrow().lang;
    set_status(lang.t("status_searching"), false);

    let app = app.clone();
    spawn_local(async move {
        let status = |key: &str| set_status(lang.t(key), false);
        let result = engine::find_candidates(&query, &levels, &status).await;
        set_busy(&app, false);
        let mut found = match result {
            Ok(found) => found,
            Err(message) => {
                set_status(&fill(lang.t("error"), &[("msg", &message)]), true);
                return;
            }
        };
        if found.is_empty() {
            set_status(lang.t("no_match"), true);
            return;
        }
        boundary::rank(&mut found, &query);
        let unambiguous = boundary::auto_pick(&found, &query);
        app.borrow_mut().candidates = found;
        set_status("", false);
        match unambiguous {
            Some(index) => select_candidate(&app, index, true),
            None => render_candidates(&app),
        }
    });
}

/// A boundary was chosen, by the user or `automatic`ally: load its polygon and its ATMs, and put
/// the place into the page address.
fn select_candidate(app: &Shared, index: usize, automatic: bool) {
    if app.borrow().busy {
        return;
    }
    let Some(mut chosen) = app.borrow().candidates.get(index).cloned() else {
        return;
    };
    let lang = app.borrow().lang;
    app.borrow_mut().candidates.clear();
    render_candidates(app);
    set_busy(app, true);
    set_status(lang.t("status_boundary"), false);

    let app = app.clone();
    spawn_local(async move {
        let status = |key: &str| set_status(lang.t(key), false);
        let result = async {
            engine::load_geometry(&mut chosen, &status).await?;
            status("status_atms");
            engine::fetch_atms(&chosen, &status).await
        }
        .await;
        set_busy(&app, false);
        let records = match result {
            Ok(records) => records,
            Err(message) => {
                set_status(&fill(lang.t("error"), &[("msg", &message)]), true);
                return;
            }
        };

        let raw_count = records.len();
        let mut atms = merge(records, MERGE_RADIUS_M);
        // Inside a municipality the municipality is the city; for a district or state we
        // cannot know, so those ATMs keep an empty city.
        if chosen.is_municipality() {
            for atm in atms.iter_mut() {
                if atm.city.is_empty() {
                    atm.city = chosen.name.clone();
                }
            }
        }
        export::sort_rows(&mut atms);
        // The name alone leads back here when it was picked automatically on any level;
        // otherwise (picked from several, or limited by the address) the level goes in too.
        let name_suffices = automatic && app.borrow().search_level.is_empty();
        let level = if name_suffices {
            ""
        } else {
            chosen.level_key()
        };
        set_page_link(&place_link(&chosen.name, level));
        {
            let mut state = app.borrow_mut();
            state.boundary = Some(chosen);
            state.atms = atms;
            state.raw_count = raw_count;
            state.fill_counts = None;
        }
        set_status("", false);
        render_results(&app);
        render_gmaps(&app);
        update_map(&app);
    });
}

/// "Complete addresses" was clicked.
fn on_fill(app: &Shared) {
    if app.borrow().busy || app.borrow().boundary.is_none() {
        return;
    }
    let lang = app.borrow().lang;
    set_busy(app, true);
    // Take the rows out of the state while the async task works on them.
    let mut atms = std::mem::take(&mut app.borrow_mut().atms);

    let app = app.clone();
    spawn_local(async move {
        let status = |key: &str| set_status(lang.t(key), false);
        let progress =
            |done: usize, total: usize| set_text("fill-progress", &format!("{done}/{total}"));
        show("fill-progress", true);
        let counts = engine::fill_addresses(&mut atms, &status, &progress).await;
        show("fill-progress", false);
        export::sort_rows(&mut atms);
        {
            let mut state = app.borrow_mut();
            state.atms = atms;
            state.fill_counts = Some(counts);
        }
        set_busy(&app, false);
        set_status("", false);
        render_results(&app);
        update_map(&app);
    });
}

/// One of the download buttons was clicked; `format` is `csv`, `geojson` or `kml`.
fn on_download(app: &Shared, format: &str) {
    let state = app.borrow();
    let Some(boundary) = &state.boundary else {
        return;
    };
    let file_base = slugify(&boundary.name);
    match format {
        "csv" => js_download(
            &format!("{file_base}.csv"),
            "text/csv;charset=utf-8",
            &export::to_csv(&state.atms),
        ),
        "geojson" => js_download(
            &format!("{file_base}.geojson"),
            "application/geo+json",
            &export::to_geojson(&state.atms, Some(boundary)),
        ),
        _ => js_download(
            &format!("{file_base}.kml"),
            "application/vnd.google-earth.kml+xml",
            &export::to_kml(&state.atms, &format!("ATMs {}", boundary.name)),
        ),
    }
}

/// "Copy link" was clicked: put the page address on the clipboard and confirm on the button.
fn on_copy_link(app: &Shared) {
    let Some(href) = web_sys::window().and_then(|window| window.location().href().ok()) else {
        return;
    };
    let promise = js_copy(&href);
    let app = app.clone();
    spawn_local(async move {
        if JsFuture::from(promise).await.is_err() {
            return;
        }
        set_text("share-button", app.borrow().lang.t("link_copied"));
        sleep(Duration::from_secs(2)).await;
        set_text("share-button", app.borrow().lang.t("copy_link"));
    });
}

/// The language toggle was clicked: switch, remember the choice, re-render everything.
fn toggle_lang(app: &Shared) {
    let next = match app.borrow().lang {
        Lang::De => Lang::En,
        Lang::En => Lang::De,
    };
    app.borrow_mut().lang = next;
    storage_set("lang", next.code());
    apply_i18n(app);
    if app.borrow().boundary.is_some() {
        update_map(app);
    }
}

/// The remembered language, else the browser language.
fn detect_lang() -> Lang {
    if let Some(code) = storage_get("lang") {
        return Lang::from_code(&code);
    }
    let browser_language = web_sys::window()
        .and_then(|window| window.navigator().language())
        .unwrap_or_default();
    Lang::from_code(&browser_language)
}

// ---------------------------------------------------------------- start

/// Entry point: translate the page, wire the events, start the map and load the ready-made index.
pub fn mount() {
    let app: Shared = Rc::new(RefCell::new(App {
        lang: detect_lang(),
        candidates: Vec::new(),
        search_level: String::new(),
        boundary: None,
        atms: Vec::new(),
        raw_count: 0,
        busy: false,
        fill_counts: None,
        ready: Vec::new(),
        lists: None,
        polling: false,
    }));
    apply_i18n(&app);

    let handler_app = app.clone();
    on(&by_id("search-form"), "submit", move |event| {
        event.prevent_default();
        on_search(&handler_app);
    });
    let handler_app = app.clone();
    on(&by_id("fill-button"), "click", move |_| {
        on_fill(&handler_app)
    });
    for format in ["csv", "geojson", "kml"] {
        let handler_app = app.clone();
        on(&by_id(&format!("dl-{format}")), "click", move |_| {
            on_download(&handler_app, format)
        });
    }
    let handler_app = app.clone();
    on(&by_id("share-button"), "click", move |_| {
        on_copy_link(&handler_app)
    });
    let handler_app = app.clone();
    on(&by_id("gmaps-create"), "click", move |_| {
        on_create_list(&handler_app)
    });
    let handler_app = app.clone();
    on(&by_id("lang-toggle"), "click", move |_| {
        toggle_lang(&handler_app)
    });

    map_init("map");
    match place_from_url() {
        Some((place, level)) => start_search(&app, &place, &level),
        None => {
            if let Some(input) = by_id("place-input").dyn_ref::<HtmlElement>() {
                input.focus().ok();
            }
        }
    }

    let task_app = app.clone();
    spawn_local(async move {
        if let Ok(entries) = engine::load_ready_index().await {
            task_app.borrow_mut().ready = entries;
            render_ready(&task_app);
        }
        if let Ok(lists) = engine::load_lists().await {
            set_lists(&task_app, lists);
        }
    });
}
