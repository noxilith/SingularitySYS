//! How the Limine menu looks: a dark panel over the wallpaper (when there is one), amber branding like the accretion
//! disk, warm light text, and a selection bar in that same light colour (Limine draws the selected entry in reverse: the
//! text colour becomes the bar and the dark panel colour becomes the lettering). The comment under the menu is cyan, which
//! the palette makes a soft blue-grey.
//! One place, used by the installed system's `limine.conf` and by the live ISO's.

/// The options at the top of `limine.conf`. `wallpaper` is a path in the form Limine takes (`boot():/wallpaper.jpg`).
pub fn limine_theme(wallpaper: Option<&str>) -> String {
    let mut s = String::from(
        "interface_branding: SingularitySYS\n\
         interface_branding_colour: e8a24b\n\
         interface_help_colour: 8b8578\n\
         interface_help_colour_bright: e8a24b\n\
         term_palette: 0b0b0d;e5604f;8bc34a;e8a24b;5aa9e6;b48ead;a9c1d9;d9d4cc\n\
         term_palette_bright: 55524c;ff7b6b;a6e063;ffc169;7ec3ff;d1a8e6;7ae8dc;ffffff\n\
         term_foreground: e6d5b8\n\
         term_foreground_bright: ffffff\n",
    );
    if let Some(w) = wallpaper {
        // Stretched to the whole screen, whatever its size (centred, it stayed 2000 pixels wide and left black bars on
        // anything bigger than 1920x1080). The menu sits in a dark translucent panel with room around it.
        s.push_str(&format!(
            "wallpaper: {w}\nwallpaper_style: stretched\nterm_background: 70060608\nterm_margin: 72\nterm_margin_gradient: 28\n"
        ));
    } else {
        s.push_str("term_background: 00060608\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_names_the_wallpaper_only_when_there_is_one() {
        let with = limine_theme(Some("boot():/wallpaper.jpg"));
        assert!(with.contains("wallpaper: boot():/wallpaper.jpg") && with.contains("wallpaper_style: stretched") && with.contains("term_margin: 72"));
        let without = limine_theme(None);
        assert!(!without.contains("wallpaper") && without.contains("interface_branding: SingularitySYS"));
    }

    #[test]
    fn colours_are_six_hex_digits_and_palettes_have_eight() {
        let t = limine_theme(None);
        for line in t.lines() {
            let (k, v) = line.split_once(": ").unwrap();
            if k.contains("palette") {
                let c: Vec<_> = v.split(';').collect();
                assert_eq!(c.len(), 8, "{k}");
                assert!(c.iter().all(|x| x.len() == 6 && x.chars().all(|d| d.is_ascii_hexdigit())), "{k}: {v}");
            } else if k.contains("colour") || k.ends_with("foreground") || k.ends_with("bright") {
                assert!(v.len() == 6 && v.chars().all(|d| d.is_ascii_hexdigit()), "{k}: {v}");
            }
        }
    }
}
