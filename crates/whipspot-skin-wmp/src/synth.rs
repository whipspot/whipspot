//! A synthetic skin built in code (hard rule 4: nothing here comes from a real skin). It has what
//! the engine has to get right: a view larger than its content, a colour-keyed background, button
//! groups with mapping and pressed images, sliders with thumbs, a hidden visualiser, texts,
//! script-sized, hidden and covered elements, an expression geometry, and a script entry that
//! cannot be inflated.
//!
//! Layout (view 300 x 200; the content box is x 20..200, y 10..125, so panel 0 is 180 x 115):
//!
//! - `svA` 20,10 120x40, art `a.bmp` with a magenta-keyed corner; in it a system group at 30,10
//!   (minimise, close, toggle-playlist, open-media) and a transport group at 30,22 (play, stop,
//!   next, previous)
//! - seek slider at `svA.left + 10`, 60 (100x10) and volume slider at 140,60 (60x10), both with
//!   thumbs
//! - `svBar` 20,80 120x24 with the title text and the time text
//! - a hidden visualiser at 150,80 (50x40)
//! - a hidden button, a button sized by a script call, and a button under `svCover`

use whipspot_skin_kit::synth::{ZEntry, ZipSpec, bmp_rgb};

type Rgb = [u8; 3];

/// Which variant of the synthetic skin to build.
#[derive(Clone, Copy)]
pub(crate) struct Opts {
    /// The system group has a `TogglePlaylist();` button.
    pub toggle_playlist: bool,
    /// `a.bmp` is a GIF (not a format this engine reads).
    pub gif_art: bool,
    /// The description has no VIEW.
    pub no_view: bool,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            toggle_playlist: true,
            gif_art: false,
            no_view: false,
        }
    }
}

fn bmp(w: usize, h: usize, f: impl Fn(usize, usize) -> Rgb) -> Vec<u8> {
    let rows: Vec<Vec<Rgb>> = (0..h).map(|y| (0..w).map(|x| f(x, y)).collect()).collect();
    bmp_rgb(24, &rows, false)
}

fn solid(w: usize, h: usize, c: Rgb) -> Vec<u8> {
    bmp(w, h, |_, _| c)
}

const MAGENTA: Rgb = [255, 0, 255];

/// A mapping bitmap: `n` blocks of `bw` x `h`, block `i` in colour `colours[i]`.
fn map(bw: usize, h: usize, colours: &[Rgb]) -> Vec<u8> {
    bmp(bw * colours.len(), h, |x, _| colours[x / bw])
}

fn utf16le(text: &str) -> Vec<u8> {
    let mut v = vec![0xff, 0xfe];
    for u in text.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

/// The `.wms` text of the skin.
fn description(o: Opts) -> String {
    if o.no_view {
        return "<THEME title=\"No View\"><PLAYER/></THEME>".to_owned();
    }
    let toggle = if o.toggle_playlist {
        "<BUTTONELEMENT mappingColor=\"#8000FF\" onClick=\"TogglePlaylist();\"/>"
    } else {
        ""
    };
    format!(
        r##"<THEME id="synth" title="Synth Test">
<VIEW id="vMain" width="300" height="200" titleBar="false" backgroundColor="none"
      scriptFile="main.js" onLoad="OnLoad();">
  <PLAYER openstatechange="OnOpenStateChange();"/>
  <SUBVIEW id="svA" left="20" top="10" width="120" height="40"
           backgroundImage="a.bmp" transparencyColor="#FF00FF">
    <BUTTONGROUP id="sys" left="10" top="0" width="80" height="10"
                 image="sys.bmp" mappingImage="sys_map.bmp">
      <BUTTONELEMENT mappingColor="#FF8000" onClick="view.minimize();"/>
      <BUTTONELEMENT mappingColor="#00FFFF" onClick="JScript:view.close();"/>
      {toggle}
      <BUTTONELEMENT mappingColor="#80FF00" onClick="OpenMedia();"/>
    </BUTTONGROUP>
    <BUTTONGROUP id="tr" left="10" top="12" width="80" height="20"
                 image="grp.bmp" downImage="grp_down.bmp" mappingImage="grp_map.bmp">
      <PLAYELEMENT mappingColor="#FF0000"/>
      <STOPELEMENT mappingColor="#00FF00"/>
      <NEXTELEMENT mappingColor="#0000FF"/>
      <PREVELEMENT mappingColor="#FFFF00"/>
    </BUTTONGROUP>
  </SUBVIEW>
  <SEEKSLIDER id="seek" left="JScript:svA.left + 10;" top="60" width="100" height="10"
              backgroundImage="seekbg.bmp" thumbImage="thumb.bmp" thumbDownImage="thumb_d.bmp"
              transparencyColor="#FF00FF"/>
  <VOLUMESLIDER id="vol" left="140" top="60" width="60" height="10"
                backgroundImage="volbg.bmp" thumbImage="thumb.bmp"/>
  <SUBVIEW id="svBar" left="20" top="80" width="120" height="24" backgroundColor="#004000">
    <TEXT id="title" left="2" top="2" width="116" foregroundColor="#00FF00"/>
    <TEXT id="time" left="60" top="12" width="56" foregroundColor="#FFFF00"
          value="wmpprop:player.controls.currentPositionString"/>
  </SUBVIEW>
  <WMPEFFECTS id="vis" left="150" top="80" width="50" height="40" visible="false"/>
  <BUTTON id="hidden" left="20" top="110" width="20" height="10" image="btn.bmp"
          visible="false" onClick="player.controls.stop()"/>
  <BUTTON id="scripted" left="45" top="110" width="JScript:GetEqSliderLeft(1);" height="10"
          onClick="player.controls.previous()"/>
  <BUTTON id="covered" left="70" top="110" width="20" height="10" image="btn.bmp"
          onClick="player.controls.next()"/>
  <SUBVIEW id="svCover" left="60" top="105" width="40" height="20" backgroundColor="#202020"/>
  <PLAYLIST id="pl" left="0" top="0" width="10" height="10" visible="false"
            backgroundcolor="#101010" foregroundcolor="#EEEEEE"/>
</VIEW>
<VIEW id="vOther" width="50" height="50"><BUTTON left="0" top="0" width="5" height="5"
      onClick="view.close();"/></VIEW>
</THEME>"##
    )
}

/// The archive of the skin: every entry deflated, in a fixed order.
pub(crate) fn skin_with(o: Opts) -> Vec<u8> {
    let a = if o.gif_art {
        b"GIF89a\x01\x00\x01\x00\x80\x00\x00".to_vec()
    } else {
        bmp(120, 40, |x, y| {
            if x < 10 && y < 10 {
                MAGENTA
            } else {
                [60, 60, 160]
            }
        })
    };
    let blocks = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
    let sys_blocks = [[255, 128, 0], [0, 255, 255], [128, 0, 255], [128, 255, 0]];
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("synth.wms", utf16le(&description(o))),
        ("a.bmp", a),
        ("grp.bmp", solid(80, 20, [100, 100, 100])),
        ("grp_down.bmp", solid(80, 20, [200, 200, 200])),
        ("grp_map.bmp", map(20, 20, &blocks)),
        ("sys.bmp", solid(80, 10, [90, 90, 90])),
        ("sys_map.bmp", map(20, 10, &sys_blocks)),
        ("seekbg.bmp", solid(100, 10, [30, 30, 30])),
        ("volbg.bmp", solid(60, 10, [30, 40, 30])),
        ("thumb.bmp", solid(8, 10, [255, 255, 0])),
        ("thumb_d.bmp", solid(8, 10, [255, 128, 0])),
        ("btn.bmp", solid(20, 10, [200, 0, 0])),
    ];
    let mut entries: Vec<ZEntry> = files.iter().map(|(n, d)| ZEntry::deflated(n, d)).collect();
    // A script whose deflate stream is damaged: reading it would fail, so the engine must not.
    let mut js = ZEntry::deflated("main.js", b"var x = 1;");
    js.payload = vec![0xff; 12];
    js.size = 64;
    entries.push(js);
    ZipSpec {
        entries,
        ..ZipSpec::default()
    }
    .bytes()
}

pub(crate) fn skin() -> Vec<u8> {
    skin_with(Opts::default())
}

/// A skin with a drawer (MD193), built in code. View 300 x 100:
///
/// - `body` 100,0 120x100, art `body.bmp`: red (`clippingColor`) in its 4 left columns, magenta
///   (`transparencyColor`) in a hole at 40..80 x 30..60, green elsewhere. Under it (zIndex -2) a
///   screen 30,20 60x50 (`tv.bmp`, black with white `clippingColor` 2 x 2 corners) holding the
///   visualiser; over it a button calling `ToggleDrawer();`.
/// - `drawer` 60,20 80x60 at zIndex -1, art `ear.bmp` (grey): a button calling `ToggleDrawer();`
///   at 2,2, and, hidden until a script opens it, a playlist at 10,10 60x30, a slider of band 3
///   at 10,45 60x10 and the label "Bal".
pub(crate) fn drawer_skin() -> Vec<u8> {
    const RED: Rgb = [255, 0, 0];
    const WHITE: Rgb = [255, 255, 255];
    let wms = r##"<THEME title="Drawer Test"><VIEW width="300" height="100" backgroundColor="none">
  <SUBVIEW left="100" top="0" backgroundImage="body.bmp" clippingColor="#FF0000"
           transparencyColor="#FF00FF">
    <SUBVIEW zIndex="-2" left="30" top="20" width="60" height="50" backgroundImage="tv.bmp"
             clippingColor="#FFFFFF">
      <EFFECTS width="60" height="50" visible="false"/>
    </SUBVIEW>
    <BUTTON left="4" top="80" image="btn.bmp" onClick="ToggleDrawer();"/>
  </SUBVIEW>
  <SUBVIEW id="drawer" zIndex="-1" left="60" top="20" width="80" height="60"
           backgroundImage="ear.bmp">
    <BUTTON left="2" top="2" image="btn.bmp" onClick="ToggleDrawer();"/>
    <PLAYLIST left="10" top="10" width="60" height="30" visible="false"/>
    <SLIDER left="10" top="45" width="60" height="10" visible="false"
            backgroundImage="volbg.bmp" value="wmpprop:eq.gainLevel3"/>
    <TEXT left="10" top="1" value="Bal" foregroundColor="#FFFF00" visible="false"/>
  </SUBVIEW>
</VIEW></THEME>"##;
    let body = bmp(120, 100, |x, y| {
        if x < 4 {
            RED
        } else if (40..80).contains(&x) && (30..60).contains(&y) {
            MAGENTA
        } else {
            [0, 200, 0]
        }
    });
    let tv = bmp(60, 50, |x, y| {
        let edge = |v: usize, n: usize| v < 2 || v >= n - 2;
        if edge(x, 60) && edge(y, 50) {
            WHITE
        } else {
            [0, 0, 0]
        }
    });
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("drawer.wms", utf16le(wms)),
        ("body.bmp", body),
        ("tv.bmp", tv),
        ("ear.bmp", solid(80, 60, [128, 128, 128])),
        ("btn.bmp", solid(10, 10, [200, 0, 0])),
        ("volbg.bmp", solid(60, 10, [30, 40, 30])),
    ];
    ZipSpec {
        entries: files.iter().map(|(n, d)| ZEntry::deflated(n, d)).collect(),
        ..ZipSpec::default()
    }
    .bytes()
}

/// An archive holding only the description `wms` (UTF-16LE), for tests of one rule.
pub(crate) fn skin_from(wms: &str) -> Vec<u8> {
    ZipSpec {
        entries: vec![ZEntry::deflated("t.wms", &utf16le(wms))],
        ..ZipSpec::default()
    }
    .bytes()
}
