//! Fake skin seams: [`FakeSkinEngine`] reads a tiny synthetic format (a magic line and a panel
//! count) and [`FakeSkinSource`] hands out in-memory files. Neither touches a file system.

use whipspot_core::skin::{
    Align, Anchor, Attach, BitmapFont, Command, Dock, Edge, Fill, ImageId, MAX_SKIN_BYTES, Panel,
    PanelId, PlayerCommand, Rect, Shape, Size, Skin, SkinEngine, SkinError, SkinImage, SkinInfo,
    SkinSource, SkinTheme, SpriteRect, Widget, WidgetKind,
};

/// The magic every sample starts with; the engine detects exactly this prefix.
const MAGIC: &[u8] = b"FAKESKIN1";
/// The panel count range the format accepts.
const PANELS: std::ops::RangeInclusive<u8> = 1..=4;
/// Every panel and the one image is this size.
const SIDE: Size = Size { w: 32, h: 16 };

/// A fake [`SkinEngine`]: id `fake`, extension `fakeskin`. A file is the magic `FAKESKIN1` and one
/// byte `n` (1..=4) for the panel count; each panel `p<i>` holds an image and a button. Panels
/// after the first attach to `p0` at its bottom edge.
#[derive(Default)]
pub struct FakeSkinEngine;

impl FakeSkinEngine {
    pub fn new() -> Self {
        Self
    }

    /// A valid file with three panels.
    pub fn sample() -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.push(3);
        bytes
    }
}

impl SkinEngine for FakeSkinEngine {
    fn id(&self) -> &'static str {
        "fake"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["fakeskin"]
    }

    fn detect(&self, bytes: &[u8]) -> bool {
        bytes.starts_with(MAGIC)
    }

    fn load(&self, bytes: &[u8]) -> Result<Skin, SkinError> {
        if bytes.len() as u64 > MAX_SKIN_BYTES {
            return Err(SkinError::TooLarge);
        }
        if bytes.len() != MAGIC.len() + 1 || !self.detect(bytes) {
            return Err(SkinError::Malformed);
        }
        let count = bytes[MAGIC.len()];
        if !PANELS.contains(&count) {
            return Err(SkinError::Malformed);
        }
        let image = SkinImage::new(SIDE.w, SIDE.h, vec![0xFF; (SIDE.w * SIDE.h * 4) as usize])
            .ok_or(SkinError::Malformed)?;
        let whole = Rect {
            x: 0,
            y: 0,
            w: SIDE.w,
            h: SIDE.h,
        };
        let button = Rect {
            x: 0,
            y: 0,
            w: 16,
            h: 8,
        };
        let panels = (0..count)
            .map(|i| Panel {
                id: PanelId::new(&format!("p{i}")).expect("fake panel ids are valid"),
                title: format!("Panel {i}"),
                size: SIDE,
                resize: None,
                shape: Shape::Rect,
                dock: if i == 0 {
                    Dock::default()
                } else {
                    Dock {
                        group: None,
                        attach: Some(Attach {
                            to: PanelId::new("p0").expect("fake panel ids are valid"),
                            edge: Edge::Bottom,
                        }),
                        snap: false,
                    }
                },
                visible_at_start: true,
                widgets: vec![
                    Widget {
                        rect: whole,
                        anchor: Anchor {
                            h: Align::Start,
                            v: Align::Start,
                        },
                        kind: WidgetKind::Image {
                            sprite: SpriteRect {
                                image: ImageId(0),
                                rect: whole,
                            },
                            fill: Fill::Once,
                        },
                    },
                    Widget {
                        rect: button,
                        anchor: Anchor {
                            h: Align::Start,
                            v: Align::Start,
                        },
                        kind: WidgetKind::Button {
                            command: Command::Player(PlayerCommand::TogglePlay),
                            normal: SpriteRect {
                                image: ImageId(0),
                                rect: button,
                            },
                            pressed: SpriteRect {
                                image: ImageId(0),
                                rect: button,
                            },
                        },
                    },
                ],
            })
            .collect();
        Ok(Skin {
            info: SkinInfo {
                engine: self.id().to_owned(),
                name: "Fake".to_owned(),
            },
            panels,
            images: vec![image],
            fonts: Vec::<BitmapFont>::new(),
            visualiser: None,
            theme: SkinTheme::default(),
        })
    }
}

/// A fake [`SkinSource`] over in-memory `(name, bytes)` entries. `list` is sorted by
/// `(lowercase, exact)` with exact duplicates dropped (the first entry wins); `read` refuses a
/// name that is empty or holds `/`, `\` or `..`, and reports `NotFound` for an unknown one.
pub struct FakeSkinSource {
    entries: Vec<(String, Vec<u8>)>,
}

impl FakeSkinSource {
    pub fn new(mut entries: Vec<(String, Vec<u8>)>) -> Self {
        entries.sort_by(|a, b| {
            a.0.to_ascii_lowercase()
                .cmp(&b.0.to_ascii_lowercase())
                .then_with(|| a.0.cmp(&b.0))
        });
        entries.dedup_by(|a, b| a.0 == b.0);
        Self { entries }
    }
}

impl SkinSource for FakeSkinSource {
    fn list(&self) -> Vec<String> {
        self.entries.iter().map(|(name, _)| name.clone()).collect()
    }

    fn read(&self, name: &str) -> Result<Vec<u8>, SkinError> {
        if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
            return Err(SkinError::InvalidName);
        }
        let (_, bytes) = self
            .entries
            .iter()
            .find(|(n, _)| n == name)
            .ok_or(SkinError::NotFound)?;
        if bytes.len() as u64 > MAX_SKIN_BYTES {
            return Err(SkinError::TooLarge);
        }
        Ok(bytes.clone())
    }
}
