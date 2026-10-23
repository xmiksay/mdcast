//! The font set glyphs are outlined with: typst's bundled fonts plus the
//! render's declared fonts — never system fonts, so output doesn't depend on
//! the host.

use std::sync::{Arc, OnceLock};

use usvg::fontdb;

/// typst's default text font, and the family usvg falls back to — its
/// default selector always appends the generic `serif` family to a query, so
/// pointing `serif` here means a `font-family` naming nothing we have (Arial,
/// trebuchet ms, ...) still outlines instead of silently vanishing, the same
/// way typst falls back to its own default font.
pub(super) const DEFAULT_FAMILY: &str = "Libertinus Serif";
pub(super) const MONOSPACE_FAMILY: &str = "DejaVu Sans Mono";

/// The per-render font set: the database plus the family unmatched and
/// generic `font-family` names resolve to.
pub(super) struct Fonts {
    pub db: Arc<fontdb::Database>,
    pub family: String,
}

/// typst's SVG text falls back to the text font active where the image
/// appears, which we can't know here; the first declared (brand) font stands
/// in for it as every generic family and the no-match fallback — otherwise a
/// branded render's diagram labels would come out in Libertinus Serif.
pub(super) fn font_db(fonts: &[Vec<u8>]) -> Fonts {
    let bundled = bundled_font_db();
    let Some(first) = fonts.first() else {
        return Fonts {
            db: bundled.clone(),
            family: DEFAULT_FAMILY.to_string(),
        };
    };
    let mut db = (**bundled).clone();
    for font in fonts {
        db.load_font_data(font.clone());
    }
    let family = family_name(first).unwrap_or_else(|| DEFAULT_FAMILY.to_string());
    db.set_serif_family(family.clone());
    db.set_sans_serif_family(family.clone());
    db.set_cursive_family(family.clone());
    db.set_fantasy_family(family.clone());
    Fonts {
        db: Arc::new(db),
        family,
    }
}

pub(super) fn family_name(font: &[u8]) -> Option<String> {
    let mut db = fontdb::Database::new();
    db.load_font_data(font.to_vec());
    let face = db.faces().next()?;
    face.families
        .iter()
        .find(|f| f.1 == fontdb::Language::English_UnitedStates)
        .or(face.families.first())
        .map(|f| f.0.clone())
}

/// Identical on every render, so built once per process.
pub(super) fn bundled_font_db() -> &'static Arc<fontdb::Database> {
    static DB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        for data in typst_assets::fonts() {
            db.load_font_source(fontdb::Source::Binary(Arc::new(data)));
        }
        db.set_serif_family(DEFAULT_FAMILY);
        db.set_monospace_family(MONOSPACE_FAMILY);
        Arc::new(db)
    })
}
