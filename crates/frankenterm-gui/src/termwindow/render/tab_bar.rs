use crate::quad::{HeapQuadAllocator, TripleLayerQuadAllocator};
use crate::termwindow::render::{LineToEleShapeCacheKey, RenderScreenLineParams};
use crate::utilsprites::RenderMetrics;
use anyhow::Context;
use config::{ConfigHandle, TabBarPosition};
use mux::renderable::RenderableDimensions;
use wezterm_term::Line;
use wezterm_term::color::{ColorAttribute, ColorPalette};
use window::color::LinearRgba;

/// Pixels that the tab bar takes from each edge of the window's
/// terminal area.  At most one of the fields is non-zero.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TabBarInsets {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

impl TabBarInsets {
    /// `horizontal_height` is the height of a top/bottom tab bar and
    /// `vertical_width` the width of a left/right tab bar, in pixels.
    pub fn new(
        position: TabBarPosition,
        show_tab_bar: bool,
        horizontal_height: f32,
        vertical_width: f32,
    ) -> Self {
        let mut insets = Self::default();
        if !show_tab_bar {
            return insets;
        }
        match position {
            TabBarPosition::Top => insets.top = horizontal_height,
            TabBarPosition::Bottom => insets.bottom = horizontal_height,
            TabBarPosition::Left => insets.left = vertical_width,
            TabBarPosition::Right => insets.right = vertical_width,
        }
        insets
    }

    /// Total width taken from the terminal area.
    pub fn width(&self) -> f32 {
        self.left + self.right
    }

    /// Total height taken from the terminal area.
    pub fn height(&self) -> f32 {
        self.top + self.bottom
    }
}

/// Quads drawn for each tab bar row, so an unchanged row is copied instead of
/// being shaped and laid out again on every frame. The vertical tab bar has a
/// row per window line, and without this cache it cost more per frame than
/// the pane itself.
#[derive(Default)]
pub struct TabBarQuadCache {
    /// Rows hold resolved colors, so a different palette drops every row.
    palette: Option<ColorPalette>,
    rows: Vec<Option<TabBarRowQuads>>,
}

impl TabBarQuadCache {
    fn start_frame(&mut self, palette: &ColorPalette, rows: usize) {
        if self.palette.as_ref() != Some(palette) {
            self.palette = Some(palette.clone());
            self.rows.clear();
        }
        self.rows.resize_with(rows, || None);
    }
}

struct TabBarRowQuads {
    key: TabBarRowQuadKey,
    layers: HeapQuadAllocator,
}

/// Everything the quads of one tab bar row depend on, apart from the palette.
#[derive(PartialEq)]
struct TabBarRowQuadKey {
    line_hash: [u8; 16],
    top_pixel_y: f32,
    left_pixel_x: f32,
    pixel_width: f32,
    cols: usize,
    cell_width: isize,
    cell_height: isize,
    config_generation: usize,
    shape_generation: usize,
    quad_generation: usize,
    window_is_transparent: bool,
    default_bg: LinearRgba,
}

impl crate::TermWindow {
    pub fn paint_tab_bar(&mut self, layers: &mut TripleLayerQuadAllocator) -> anyhow::Result<()> {
        if self.config.is_vertical_tab_bar() {
            // The vertical tab bar is always drawn in the retro style,
            // including when use_fancy_tab_bar is set.
            return self.paint_vertical_tab_bar(layers);
        }

        if self.config.use_fancy_tab_bar {
            if self.fancy_tab_bar.is_none() {
                let palette = self.palette().clone();
                let tab_bar = self.build_fancy_tab_bar(&palette)?;
                self.fancy_tab_bar.replace(tab_bar);
            }

            self.ui_items.append(&mut self.paint_fancy_tab_bar()?);
            return Ok(());
        }

        let border = self.get_os_border();

        let palette = self.palette().clone();
        let tab_bar_height = self.tab_bar_pixel_height()?;
        let tab_bar_y = if self.config.is_tab_bar_at_bottom() {
            ((self.dimensions.pixel_height as f32) - (tab_bar_height + border.bottom.get() as f32))
                .max(0.)
        } else {
            border.top.get() as f32
        };

        // Register the tab bar location
        self.ui_items.append(&mut self.tab_bar.compute_ui_items(
            0,
            tab_bar_y as usize,
            self.render_metrics.cell_size.width as usize,
            self.render_metrics.cell_size.height as usize,
        ));

        let cols =
            self.dimensions.pixel_width / (self.render_metrics.cell_size.width as usize).max(1);
        self.tab_bar_quad_cache.borrow_mut().start_frame(&palette, 1);
        self.render_tab_bar_line(
            layers,
            &palette,
            0,
            self.tab_bar.line(),
            0.,
            tab_bar_y,
            self.dimensions.pixel_width as f32,
            cols,
        )
    }

    fn paint_vertical_tab_bar(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let border = self.get_os_border();
        let palette = self.palette().clone();
        let tab_bar_width = self.vertical_tab_bar_pixel_width();
        let cell_height = self.render_metrics.cell_size.height as f32;

        let left_pixel_x = self.vertical_tab_bar_left_pixel_x();
        let top_pixel_y = border.top.get() as f32;

        self.ui_items.append(&mut self.tab_bar.compute_ui_items(
            left_pixel_x as usize,
            top_pixel_y as usize,
            self.render_metrics.cell_size.width as usize,
            self.render_metrics.cell_size.height as usize,
        ));

        let cols = self.config.vertical_tab_width.max(1);
        let lines = self.tab_bar.vertical_lines();
        self.tab_bar_quad_cache
            .borrow_mut()
            .start_frame(&palette, lines.len());
        for (row, line) in lines.iter().enumerate() {
            self.render_tab_bar_line(
                layers,
                &palette,
                row,
                line,
                left_pixel_x,
                top_pixel_y + (row as f32 * cell_height),
                tab_bar_width,
                cols,
            )?;
        }

        // The bar holds whole rows only, so the window height is rarely an
        // exact multiple of the cell height. Paint the leftover strip under
        // the last row in the bar's background, or a transparent window
        // shows through it. The alpha matches the rows above: they draw the
        // same non-default background at text_background_opacity.
        let rows_bottom = top_pixel_y + (lines.len() as f32 * cell_height);
        let bar_bottom = self.dimensions.pixel_height as f32 - border.bottom.get() as f32;
        if bar_bottom > rows_bottom {
            let colors = self
                .config
                .resolved_palette
                .tab_bar
                .clone()
                .unwrap_or_default();
            let background = palette
                .resolve_bg(ColorAttribute::TrueColorWithDefaultFallback(
                    *colors.background(),
                ))
                .to_linear()
                .mul_alpha(self.config.text_background_opacity);
            self.filled_rectangle(
                layers,
                0,
                euclid::rect(
                    left_pixel_x,
                    rows_bottom,
                    tab_bar_width,
                    bar_bottom - rows_bottom,
                ),
                background,
            )
            .context("fill vertical tab bar below the last row")?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn render_tab_bar_line(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        palette: &ColorPalette,
        row: usize,
        line: &Line,
        left_pixel_x: f32,
        top_pixel_y: f32,
        pixel_width: f32,
        cols: usize,
    ) -> anyhow::Result<()> {
        let window_is_transparent =
            !self.window_background.is_empty() || self.config.window_background_opacity != 1.0;
        let gl_state = self
            .render_state
            .as_ref()
            .context("render state is not initialized")?;
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();
        let default_bg = palette
            .resolve_bg(ColorAttribute::Default)
            .to_linear()
            .mul_alpha(if window_is_transparent {
                0.
            } else {
                self.config.text_background_opacity
            });

        let line_hash = tab_bar_shape_hash(line);
        let key = TabBarRowQuadKey {
            line_hash,
            top_pixel_y,
            left_pixel_x,
            pixel_width,
            cols,
            cell_width: self.render_metrics.cell_size.width,
            cell_height: self.render_metrics.cell_size.height,
            config_generation: self.config.generation(),
            shape_generation: self.shape_generation,
            quad_generation: self.quad_generation,
            window_is_transparent,
            default_bg,
        };
        if let Some(Some(cached)) = self.tab_bar_quad_cache.borrow().rows.get(row) {
            if cached.key == key {
                return cached
                    .layers
                    .apply_to(layers)
                    .context("apply cached tab bar row quads");
            }
        }

        let mut buf = HeapQuadAllocator::default();
        self.render_screen_line(
            RenderScreenLineParams {
                top_pixel_y,
                left_pixel_x,
                pixel_width,
                stable_line_idx: None,
                line,
                selection: 0..0,
                cursor: &Default::default(),
                palette,
                dims: &RenderableDimensions {
                    cols,
                    physical_top: 0,
                    scrollback_rows: 0,
                    scrollback_top: 0,
                    viewport_rows: 1,
                    dpi: self.terminal_size.dpi,
                    pixel_height: self.render_metrics.cell_size.height as usize,
                    pixel_width: pixel_width as usize,
                    reverse_video: false,
                },
                config: &self.config,
                cursor_border_color: LinearRgba::default(),
                foreground: palette.foreground.to_linear(),
                pane: None,
                is_active: true,
                selection_fg: LinearRgba::default(),
                selection_bg: LinearRgba::default(),
                cursor_fg: LinearRgba::default(),
                cursor_bg: LinearRgba::default(),
                cursor_is_default_color: true,
                white_space,
                filled_box,
                window_is_transparent,
                default_bg,
                style: None,
                font: None,
                use_pixel_positioning: self.config.experimental_pixel_positioning,
                render_metrics: self.render_metrics,
                shape_key: Some(LineToEleShapeCacheKey {
                    shape_hash: line_hash,
                    composing: None,
                    shape_generation: self.shape_generation,
                }),
                password_input: false,
            },
            &mut TripleLayerQuadAllocator::Heap(&mut buf),
        )?;
        buf.apply_to(layers)
            .context("apply tab bar row quads")?;

        if let Some(slot) = self.tab_bar_quad_cache.borrow_mut().rows.get_mut(row) {
            *slot = Some(TabBarRowQuads { key, layers: buf });
        }
        Ok(())
    }

    pub fn tab_bar_pixel_height_impl(
        config: &ConfigHandle,
        fontconfig: &frankenterm_font::FontConfiguration,
        render_metrics: &RenderMetrics,
    ) -> anyhow::Result<f32> {
        if config.use_fancy_tab_bar {
            let font = fontconfig.title_font()?;
            Ok((font.metrics().cell_height.get() as f32 * 1.75).ceil())
        } else {
            Ok(render_metrics.cell_size.height as f32)
        }
    }

    /// Height of a horizontal (top or bottom) tab bar, in pixels.
    pub fn tab_bar_pixel_height(&self) -> anyhow::Result<f32> {
        Self::tab_bar_pixel_height_impl(&self.config, &self.fonts, &self.render_metrics)
    }

    /// Width of a vertical (left or right) tab bar, in pixels.
    pub fn vertical_tab_bar_pixel_width_impl(
        config: &ConfigHandle,
        render_metrics: &RenderMetrics,
    ) -> f32 {
        (config.vertical_tab_width * render_metrics.cell_size.width as usize) as f32
    }

    pub fn vertical_tab_bar_pixel_width(&self) -> f32 {
        Self::vertical_tab_bar_pixel_width_impl(&self.config, &self.render_metrics)
    }

    /// X coordinate of the left edge of the vertical tab bar.
    pub fn vertical_tab_bar_left_pixel_x(&self) -> f32 {
        let border = self.get_os_border();
        match self.config.effective_tab_bar_position() {
            TabBarPosition::Right => (self.dimensions.pixel_width as f32
                - self.vertical_tab_bar_pixel_width()
                - border.right.get() as f32)
                .max(0.),
            _ => border.left.get() as f32,
        }
    }

    pub fn tab_bar_insets_impl(
        config: &ConfigHandle,
        show_tab_bar: bool,
        fontconfig: &frankenterm_font::FontConfiguration,
        render_metrics: &RenderMetrics,
    ) -> anyhow::Result<TabBarInsets> {
        let position = config.effective_tab_bar_position();
        if !show_tab_bar {
            return Ok(TabBarInsets::default());
        }
        let (height, width) = if config.is_vertical_tab_bar() {
            (
                0.,
                Self::vertical_tab_bar_pixel_width_impl(config, render_metrics),
            )
        } else {
            (
                Self::tab_bar_pixel_height_impl(config, fontconfig, render_metrics)?,
                0.,
            )
        };
        Ok(TabBarInsets::new(position, show_tab_bar, height, width))
    }

    /// The space the tab bar currently takes from each edge of the
    /// terminal area.  All zero when the tab bar is hidden.
    pub fn tab_bar_insets(&self) -> anyhow::Result<TabBarInsets> {
        Self::tab_bar_insets_impl(
            &self.config,
            self.show_tab_bar,
            &self.fonts,
            &self.render_metrics,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insets_follow_position() {
        let top = TabBarInsets::new(TabBarPosition::Top, true, 20., 200.);
        assert_eq!(
            top,
            TabBarInsets {
                top: 20.,
                ..Default::default()
            }
        );
        let bottom = TabBarInsets::new(TabBarPosition::Bottom, true, 20., 200.);
        assert_eq!(bottom.bottom, 20.);
        assert_eq!(bottom.height(), 20.);
        assert_eq!(bottom.width(), 0.);

        let left = TabBarInsets::new(TabBarPosition::Left, true, 20., 200.);
        assert_eq!(
            left,
            TabBarInsets {
                left: 200.,
                ..Default::default()
            }
        );
        assert_eq!(left.width(), 200.);
        assert_eq!(left.height(), 0.);

        let right = TabBarInsets::new(TabBarPosition::Right, true, 20., 200.);
        assert_eq!(right.right, 200.);
        assert_eq!(right.width(), 200.);
    }

    #[test]
    fn hidden_tab_bar_takes_no_space() {
        for position in [
            TabBarPosition::Top,
            TabBarPosition::Bottom,
            TabBarPosition::Left,
            TabBarPosition::Right,
        ] {
            assert_eq!(
                TabBarInsets::new(position, false, 20., 200.),
                TabBarInsets::default()
            );
        }
    }
}

/// The line shape cache key for a tab bar row. Without one, every frame
/// shaped every row of the tab bar again, and with a vertical tab bar of a
/// few dozen tabs that cost more than drawing the panes. Tab bar rows are
/// colored with the window palette while pane rows can use their own, so the
/// row hash is salted to keep tab bar entries apart from pane entries with the
/// same text and attributes.
fn tab_bar_shape_hash(line: &Line) -> [u8; 16] {
    const TAB_BAR_SALT: [u8; 16] = *b"frankenterm-tabs";
    let mut hash = line.compute_shape_hash();
    for (byte, salt) in hash.iter_mut().zip(TAB_BAR_SALT) {
        *byte ^= salt;
    }
    hash
}


#[cfg(test)]
mod tab_bar_shape_tests {
    use super::*;
    use termwiz::cell::CellAttributes;

    fn row(text: &str) -> Line {
        Line::from_text(text, &CellAttributes::default(), 0, None)
    }

    #[test]
    fn tab_bar_rows_get_stable_keys_apart_from_pane_rows() {
        let title = row(" 1 ● build ");
        assert_eq!(tab_bar_shape_hash(&title), tab_bar_shape_hash(&row(" 1 ● build ")));
        assert_ne!(tab_bar_shape_hash(&title), tab_bar_shape_hash(&row(" 2 ● build ")));
        assert_ne!(
            tab_bar_shape_hash(&title),
            title.compute_shape_hash(),
            "a pane row with the same text must not share the tab bar entry"
        );
    }

    fn cached_row(cache: &mut TabBarQuadCache, row: usize) {
        cache.rows[row] = Some(TabBarRowQuads {
            key: TabBarRowQuadKey {
                line_hash: [0; 16],
                top_pixel_y: 0.,
                left_pixel_x: 0.,
                pixel_width: 0.,
                cols: 0,
                cell_width: 0,
                cell_height: 0,
                config_generation: 0,
                shape_generation: 0,
                quad_generation: 0,
                window_is_transparent: false,
                default_bg: LinearRgba::default(),
            },
            layers: HeapQuadAllocator::default(),
        });
    }

    #[test]
    fn tab_bar_quad_cache_keeps_rows_until_the_palette_changes() {
        let palette = ColorPalette::default();
        let mut cache = TabBarQuadCache::default();
        cache.start_frame(&palette, 3);
        cached_row(&mut cache, 0);
        cached_row(&mut cache, 2);

        cache.start_frame(&palette, 3);
        assert!(cache.rows[0].is_some() && cache.rows[2].is_some());

        cache.start_frame(&palette, 2);
        assert_eq!(cache.rows.len(), 2, "rows past the bar's height are dropped");
        assert!(cache.rows[0].is_some());

        let mut other = palette.clone();
        other.foreground = wezterm_term::color::SrgbaTuple(1., 0., 0., 1.);
        cache.start_frame(&other, 2);
        assert!(
            cache.rows.iter().all(Option::is_none),
            "rows hold resolved colors, so a new palette must drop them"
        );
    }
}
