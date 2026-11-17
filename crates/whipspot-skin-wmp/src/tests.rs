//! Engine tests: the contract suites on the synthetic skin, what the loaded skin holds, and the
//! ignored check of a real sample named by `WHIPSPOT_WMZ_SAMPLE`.

use std::sync::Arc;

use whipspot_core::skin::{
    Axis, Command, Edge, ImageId, PlayerCommand, Rect, Rgb, Shape, Size, Skin, SkinEngine,
    SkinError, SkinRegistry, SkinSource, TextRead, Value, Widget, WidgetKind, WindowCommand,
};
use whipspot_core::skin::{Panel, PanelId};
use whipspot_testkit::{run_skin_caps_contract, run_skin_engine_contract};

use crate::WmpEngine;
use crate::synth::{self, Opts};
use crate::widgets::{library_id, playlist_id};

fn load(bytes: &[u8]) -> Skin {
    let skin = WmpEngine.load(bytes).expect("the synthetic skin loads");
    assert_eq!(skin.validate(), Ok(()));
    skin
}

fn buttons(p: &Panel) -> Vec<Command> {
    p.widgets
        .iter()
        .filter_map(|w| match &w.kind {
            WidgetKind::Button { command, .. } => Some(command.clone()),
            _ => None,
        })
        .collect()
}

fn count(p: &Panel, f: impl Fn(&WidgetKind) -> bool) -> usize {
    p.widgets.iter().filter(|w| f(&w.kind)).count()
}

fn slider(p: &Panel, v: Value) -> Option<&Widget> {
    p.widgets
        .iter()
        .find(|w| matches!(&w.kind, WidgetKind::Slider { value, .. } if *value == v))
}

struct One(Vec<u8>);

impl SkinSource for One {
    fn list(&self) -> Vec<String> {
        vec!["synth.wmz".to_owned()]
    }

    fn read(&self, _: &str) -> Result<Vec<u8>, SkinError> {
        Ok(self.0.clone())
    }
}

#[test]
fn engine_contract_suite() {
    run_skin_engine_contract(&WmpEngine, &synth::skin());
}

#[test]
fn caps_contract_on_the_synthetic_skin() {
    run_skin_caps_contract(&load(&synth::skin()));
}

#[test]
fn identity_and_detect() {
    assert_eq!(WmpEngine.id(), "wmz");
    assert_eq!(WmpEngine.extensions(), &["wmz"]);
    assert!(WmpEngine.detect(&synth::skin()));
    assert!(!WmpEngine.detect(&[]));
    assert!(!WmpEngine.detect(&[0; 4096]));
    // An archive with no description is not a skin of this format.
    let other = whipspot_skin_kit::synth::zip_deflated(&[("main.bmp", b"x")]);
    assert!(!WmpEngine.detect(&other));
    assert_eq!(WmpEngine.load(&other), Err(SkinError::Malformed));
}

/// The first painted widget (after the DragArea, which draws nothing) is the whole body art.
fn assert_body_art_first(skin: &Skin) {
    let main = &skin.panels[0];
    let first = main
        .widgets
        .iter()
        .find(|w| !matches!(w.kind, WidgetKind::DragArea))
        .expect("a painted widget");
    let full = Rect {
        x: 0,
        y: 0,
        w: main.size.w,
        h: main.size.h,
    };
    assert_eq!(first.rect, full);
    match &first.kind {
        WidgetKind::Image { sprite, .. } => {
            assert_eq!(sprite.image, ImageId(0));
            assert_eq!(sprite.rect, full);
        }
        other => panic!("the first painted widget is {other:?}"),
    }
    let full_images = count(
        main,
        |k| matches!(k, WidgetKind::Image { sprite, .. } if sprite.rect == full),
    );
    assert_eq!(full_images, 1);
    assert!(
        skin.visualiser.is_some(),
        "the skin has a visualiser palette"
    );
}

#[test]
fn panel_0_draws_its_body_art_and_has_a_visualiser_palette() {
    let skin = load(&synth::skin());
    assert_body_art_first(&skin);
    let vis = skin.visualiser.as_ref().expect("palette");
    assert!(!vis.bars.is_empty() && !vis.scope.is_empty());
    assert_eq!(vis.grid, vis.background);
}

#[test]
fn panel_0_has_exactly_the_expected_commands() {
    let skin = load(&synth::skin());
    let mut got = buttons(&skin.panels[0]);
    let mut want = vec![
        Command::Player(PlayerCommand::TogglePlay),
        Command::Player(PlayerCommand::Stop),
        Command::Player(PlayerCommand::Next),
        Command::Player(PlayerCommand::Previous),
        Command::Window(WindowCommand::Minimize),
        Command::Window(WindowCommand::ClosePanel),
        Command::Window(WindowCommand::TogglePanel(playlist_id())),
    ];
    let key = |c: &Command| format!("{c:?}");
    got.sort_by_key(key);
    want.sort_by_key(key);
    // The hidden, script-sized and covered buttons, and OpenMedia(), add nothing.
    assert_eq!(got, want);
}

#[test]
fn panel_0_widgets_and_geometry() {
    let skin = load(&synth::skin());
    let main = &skin.panels[0];
    assert_eq!(main.id.as_str(), "main");
    assert_eq!(main.title, "Synth Test");
    assert_eq!(main.size, Size { w: 180, h: 115 });
    assert!(main.resize.is_none() && main.visible_at_start);
    assert!(matches!(main.shape, Shape::Polygons(_)));
    assert_eq!(count(main, |k| matches!(k, WidgetKind::Visualiser)), 1);
    assert_eq!(count(main, |k| matches!(k, WidgetKind::DragArea)), 1);
    assert!(matches!(main.widgets[0].kind, WidgetKind::DragArea));
    let titles = count(main, |k| {
        matches!(
            k,
            WidgetKind::Text {
                text: TextRead::TrackTitle,
                scroll: true,
                ..
            }
        )
    });
    assert_eq!(titles, 1);
    let digits = count(main, |k| {
        matches!(
            k,
            WidgetKind::Text {
                text: TextRead::Time(_),
                ..
            }
        )
    });
    assert_eq!(digits, 4);
    // The colon between the clock digits and the body art.
    assert_eq!(count(main, |k| matches!(k, WidgetKind::Image { .. })), 2);
    // The seek slider sits at `svA.left + 10` = 30, top 60, in the cropped panel (x 20, y 10).
    let seek = slider(main, Value::Position).expect("seek slider");
    assert_eq!(
        seek.rect,
        Rect {
            x: 10,
            y: 50,
            w: 100,
            h: 10
        }
    );
    for v in [Value::Position, Value::Volume] {
        match &slider(main, v).expect("slider").kind {
            WidgetKind::Slider {
                thumb: Some(t),
                axis,
                ..
            } => {
                assert_eq!(*axis, Axis::Horizontal);
                assert_eq!((t[0].rect.w, t[0].rect.h), (8, 10));
            }
            other => panic!("slider with a thumb expected: {other:?}"),
        }
    }
    // The transport group has a down image, so there is a pressed layer after the main layer.
    assert_eq!(skin.images[0].width(), 180);
    assert_eq!(skin.images[1].width(), 180);
    let play = main
        .widgets
        .iter()
        .find_map(|w| match &w.kind {
            WidgetKind::Button {
                command: Command::Player(PlayerCommand::TogglePlay),
                normal,
                pressed,
            } => Some((normal, pressed)),
            _ => None,
        })
        .expect("play");
    assert_eq!((play.0.image, play.1.image), (ImageId(0), ImageId(1)));
    assert_eq!(play.0.rect, play.1.rect);
    // The pressed pixels differ from the normal ones under the play button.
    let at = |img: usize, r: Rect| {
        let im = &skin.images[img];
        let o = ((r.y * im.width() + r.x) * 4) as usize;
        im.rgba()[o..o + 3].to_vec()
    };
    assert_ne!(at(0, play.0.rect), at(1, play.0.rect));
}

#[test]
fn panels_are_main_playlist_library_and_reachable() {
    let skin = load(&synth::skin());
    let ids: Vec<&str> = skin.panels.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["main", "playlist", "library"]);
    let playlist = &skin.panels[1];
    let library = &skin.panels[2];
    assert!(playlist.visible_at_start && !library.visible_at_start);
    // The main panel toggles the playlist, so the playlist may be closed; its bar toggles the
    // library, which can be closed again.
    assert!(buttons(playlist).contains(&Command::Window(WindowCommand::ClosePanel)));
    assert!(buttons(playlist).contains(&Command::Window(WindowCommand::TogglePanel(library_id()))));
    assert!(buttons(library).contains(&Command::Window(WindowCommand::ClosePanel)));
    assert_eq!(count(playlist, |k| matches!(k, WidgetKind::TrackList)), 1);
    assert_eq!(count(library, |k| matches!(k, WidgetKind::Library)), 1);
    assert_eq!(
        playlist.dock.attach.as_ref().map(|a| a.edge),
        Some(Edge::Bottom)
    );
    assert_eq!(
        library.dock.attach.as_ref().map(|a| a.edge),
        Some(Edge::Right)
    );
    assert_eq!(
        skin.theme.list_background,
        Some(Rgb {
            r: 16,
            g: 16,
            b: 16
        })
    );
    assert_eq!(
        skin.theme.list_text,
        Some(Rgb {
            r: 238,
            g: 238,
            b: 238
        })
    );
}

#[test]
fn without_a_playlist_toggle_the_playlist_cannot_be_closed() {
    let skin = load(&synth::skin_with(Opts {
        toggle_playlist: false,
        ..Opts::default()
    }));
    let playlist = &skin.panels[1];
    assert!(playlist.visible_at_start);
    assert!(!buttons(playlist).contains(&Command::Window(WindowCommand::ClosePanel)));
    assert!(
        !buttons(&skin.panels[0])
            .iter()
            .any(|c| matches!(c, Command::Window(WindowCommand::TogglePanel(_))))
    );
}

#[test]
fn art_in_a_format_it_cannot_read_is_a_missing_image() {
    let skin = load(&synth::skin_with(Opts {
        gif_art: true,
        ..Opts::default()
    }));
    // The element is drawn without its picture; its buttons are still there.
    assert_eq!(buttons(&skin.panels[0]).len(), 7);
}

#[test]
fn a_description_without_a_view_is_an_error() {
    let bytes = synth::skin_with(Opts {
        no_view: true,
        ..Opts::default()
    });
    assert_eq!(WmpEngine.load(&bytes), Err(SkinError::Malformed));
}

#[test]
fn the_registry_loads_it_by_extension() {
    let mut reg = SkinRegistry::new();
    reg.register(Arc::new(WmpEngine)).unwrap();
    assert_eq!(reg.extensions(), ["wmz"]);
    let skin = reg.load(&One(synth::skin()), "synth.wmz").unwrap();
    assert_eq!(skin.info.engine, "wmz");
    assert_eq!(skin.info.name, "synth");
}

#[test]
fn a_flood_of_visualisers_runs_out_of_budget_instead_of_time() {
    let mut wms = String::from("<THEME><VIEW width=\"2048\" height=\"2048\">");
    for _ in 0..4000 {
        wms.push_str("<EFFECTS width=\"2048\" height=\"2048\"/>");
    }
    wms.push_str("</VIEW></THEME>");
    assert_eq!(
        WmpEngine.load(&synth::skin_from(&wms)),
        Err(SkinError::TooLarge)
    );
}

#[test]
fn a_clock_field_at_the_bottom_edge_is_left_out_not_fatal() {
    let wms = "<THEME><VIEW width=\"100\" height=\"10\">\
        <SUBVIEW width=\"100\" height=\"10\" backgroundColor=\"#004000\">\
        <TEXT left=\"0\" top=\"4\" width=\"60\" height=\"6\" \
        value=\"wmpprop:player.controls.currentPositionString\"/>\
        </SUBVIEW></VIEW></THEME>";
    let skin = load(&synth::skin_from(wms));
    let digits = count(&skin.panels[0], |k| {
        matches!(
            k,
            WidgetKind::Text {
                text: TextRead::Time(_),
                ..
            }
        )
    });
    assert_eq!(digits, 0);
}

/// Loads the user's sample from outside the repository (run by hand with `--ignored`).
#[test]
#[ignore = "needs WHIPSPOT_WMZ_SAMPLE to name a .wmz file"]
fn loads_the_sample_from_env() {
    let Ok(path) = std::env::var("WHIPSPOT_WMZ_SAMPLE") else {
        return;
    };
    let bytes = std::fs::read(path).expect("the sample can be read");
    let mut reg = SkinRegistry::new();
    reg.register(Arc::new(WmpEngine)).unwrap();
    let skin = reg
        .load(&One(bytes), "synth.wmz")
        .expect("the sample loads");
    let main = &skin.panels[0];
    let cmds = buttons(main);
    for want in [
        Command::Player(PlayerCommand::TogglePlay),
        Command::Player(PlayerCommand::Stop),
        Command::Player(PlayerCommand::Next),
        Command::Player(PlayerCommand::Previous),
        Command::Window(WindowCommand::Minimize),
        Command::Window(WindowCommand::ClosePanel),
        Command::Window(WindowCommand::TogglePanel(playlist_id())),
    ] {
        assert!(cmds.contains(&want), "missing {want:?}");
    }
    assert!(slider(main, Value::Position).is_some());
    assert!(slider(main, Value::Volume).is_some());
    assert_eq!(count(main, |k| matches!(k, WidgetKind::Visualiser)), 1);
    assert_body_art_first(&skin);
    let titles = count(main, |k| {
        matches!(
            k,
            WidgetKind::Text {
                text: TextRead::TrackTitle,
                ..
            }
        )
    });
    assert_eq!(titles, 1);
    let digits = count(main, |k| {
        matches!(
            k,
            WidgetKind::Text {
                text: TextRead::Time(_),
                ..
            }
        )
    });
    assert_eq!(digits, 4);
    assert!(matches!(main.shape, Shape::Polygons(_)));
    // Nothing interactive lies inside the visualiser rectangle.
    let vis = main
        .widgets
        .iter()
        .find(|w| matches!(w.kind, WidgetKind::Visualiser))
        .expect("visualiser")
        .rect;
    let inside = |r: Rect| {
        r.x < vis.x + vis.w && vis.x < r.x + r.w && r.y < vis.y + vis.h && vis.y < r.y + r.h
    };
    let stray = main
        .widgets
        .iter()
        .filter(|w| {
            matches!(
                w.kind,
                WidgetKind::Button { .. } | WidgetKind::Slider { .. }
            ) && inside(w.rect)
        })
        .count();
    assert_eq!(stray, 0, "interactive widgets inside the visualiser");
    let (polygons, points) = match &main.shape {
        Shape::Polygons(p) => (p.len(), p.iter().map(Vec::len).sum::<usize>()),
        Shape::Rect => (0, 0),
    };
    println!(
        "sample: panel0 {}x{} widgets={} buttons={} sliders={} texts={} polygons={polygons} points={points} images={} fonts={} panels={}",
        main.size.w,
        main.size.h,
        main.widgets.len(),
        cmds.len(),
        count(main, |k| matches!(k, WidgetKind::Slider { .. })),
        count(main, |k| matches!(k, WidgetKind::Text { .. })),
        skin.images.len(),
        skin.fonts.len(),
        skin.panels.len(),
    );
    println!("sample buttons: {cmds:?}");
}

fn panel<'s>(skin: &'s Skin, id: &str) -> &'s Panel {
    skin.panels
        .iter()
        .find(|p| p.id.as_str() == id)
        .unwrap_or_else(|| panic!("no panel {id}"))
}

/// The RGBA of the pixel at `(x, y)` of image `image`.
fn pixel(skin: &Skin, image: ImageId, x: u32, y: u32) -> [u8; 4] {
    let im = &skin.images[usize::from(image.0)];
    let o = ((y * im.width() + x) * 4) as usize;
    let p = &im.rgba()[o..o + 4];
    [p[0], p[1], p[2], p[3]]
}

/// The image the panel's body art comes from.
fn layer(p: &Panel) -> ImageId {
    p.widgets
        .iter()
        .find_map(|w| match &w.kind {
            WidgetKind::Image { sprite, .. } if w.rect.x == 0 && w.rect.y == 0 => {
                Some(sprite.image)
            }
            _ => None,
        })
        .expect("body art")
}

#[test]
fn a_drawer_becomes_a_closed_and_an_open_panel_its_buttons_swap() {
    let skin = load(&synth::drawer_skin());
    let ids: Vec<&str> = skin.panels.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        ["main", "drawer-1", "drawer-1-open", "playlist", "library"]
    );
    let swap = Command::Window(WindowCommand::SwapPanels(
        PanelId::new("drawer-1-open").unwrap(),
        PanelId::new("drawer-1").unwrap(),
    ));
    let (main, closed, open) = (
        panel(&skin, "main"),
        panel(&skin, "drawer-1"),
        panel(&skin, "drawer-1-open"),
    );
    // Both colour keys are cut: the red columns are not part of the body.
    assert_eq!(main.size, Size { w: 116, h: 100 });
    // The body's own button and the drawer's button swap the drawer's two panels.
    assert_eq!(buttons(main), std::slice::from_ref(&swap));
    assert_eq!(buttons(closed), std::slice::from_ref(&swap));
    assert_eq!(buttons(open), [swap]);
    // Closed: the part of the drawer left of the body, from the body's top down. Open: slid
    // out flush with the view's left edge, with what its script would show.
    assert_eq!(closed.size, Size { w: 44, h: 80 });
    assert_eq!(open.size, Size { w: 104, h: 80 });
    assert!(closed.visible_at_start && !open.visible_at_start);
    for p in [closed, open] {
        let attach = p.dock.attach.as_ref().expect("attached");
        assert_eq!((attach.to.as_str(), attach.edge), ("main", Edge::Left));
        assert_eq!(p.dock.group.as_deref(), Some("main"));
        assert_eq!(count(p, |k| matches!(k, WidgetKind::DragArea)), 0);
    }
    assert_eq!(count(closed, |k| matches!(k, WidgetKind::TrackList)), 0);
    let list = open
        .widgets
        .iter()
        .find(|w| matches!(w.kind, WidgetKind::TrackList))
        .expect("the skin's own playlist");
    assert_eq!(
        list.rect,
        Rect {
            x: 10,
            y: 30,
            w: 60,
            h: 30
        }
    );
    assert!(slider(open, Value::EqBand(2)).is_some());
    assert!(slider(closed, Value::EqBand(2)).is_none());
    // The label is drawn in its colour in the open picture: the "B" at 10,21 has a yellow pixel.
    let art = layer(open);
    let yellow = (10..16)
        .flat_map(|x| (21..29).map(move |y| (x, y)))
        .any(|(x, y)| pixel(&skin, art, x, y) == [255, 255, 0, 255]);
    assert!(yellow, "the label is drawn");
    // The skin shows the queue itself, so the composed playlist starts hidden and can close.
    let playlist = panel(&skin, "playlist");
    assert!(!playlist.visible_at_start);
    assert!(buttons(playlist).contains(&Command::Window(WindowCommand::ClosePanel)));
}

#[test]
fn the_body_draws_over_the_visualiser_round_its_hole() {
    let skin = load(&synth::drawer_skin());
    let main = panel(&skin, "main");
    let at = main
        .widgets
        .iter()
        .position(|w| matches!(w.kind, WidgetKind::Visualiser))
        .expect("visualiser");
    let vis = main.widgets[at].rect;
    // The screen at 30,20 of the body, the body cropped from its fifth column.
    assert_eq!(
        vis,
        Rect {
            x: 26,
            y: 20,
            w: 60,
            h: 50
        }
    );
    // Right after it, the body's art over it: opaque on the rim, clear in the hole.
    let over = &main.widgets[at + 1];
    assert_eq!(over.rect, vis);
    let WidgetKind::Image { sprite, .. } = &over.kind else {
        panic!("an overlay image follows the visualiser");
    };
    assert_eq!(pixel(&skin, sprite.image, 2, 2), [0, 200, 0, 255]);
    assert_eq!(pixel(&skin, sprite.image, 30, 25)[3], 0);
    // Under the hole the screen is painted (the body's own layer shows it black).
    assert_eq!(pixel(&skin, layer(main), 26 + 30, 20 + 25), [0, 0, 0, 255]);
}

#[test]
fn without_a_drawer_the_panels_are_unchanged() {
    // A subview whose buttons call script functions but that is not tucked behind (zIndex 0) is
    // body, not a drawer.
    let wms = "<THEME><VIEW width=\"100\" height=\"50\">\
        <SUBVIEW id=\"a\" width=\"100\" height=\"50\" backgroundColor=\"#004000\">\
        <BUTTON left=\"0\" top=\"0\" width=\"10\" height=\"10\" onClick=\"Toggle();\"/>\
        </SUBVIEW></VIEW></THEME>";
    let skin = load(&synth::skin_from(wms));
    let ids: Vec<&str> = skin.panels.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["main", "playlist", "library"]);
    assert!(buttons(&skin.panels[0]).is_empty());
}

#[test]
fn a_drawer_with_nothing_beside_the_body_stays_in_it() {
    // The drawer lies wholly under the body: no panel of its own, and its call stays inert.
    let wms = "<THEME><VIEW width=\"100\" height=\"50\">\
        <SUBVIEW width=\"100\" height=\"50\" backgroundColor=\"#004000\"/>\
        <SUBVIEW id=\"d\" zIndex=\"-1\" left=\"10\" top=\"10\" width=\"20\" height=\"20\" \
        backgroundColor=\"#808080\">\
        <BUTTON left=\"0\" top=\"0\" width=\"10\" height=\"10\" onClick=\"Toggle();\"/>\
        </SUBVIEW></VIEW></THEME>";
    let skin = load(&synth::skin_from(wms));
    let ids: Vec<&str> = skin.panels.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["main", "playlist", "library"]);
    assert!(skin.panels[1].visible_at_start);
}
