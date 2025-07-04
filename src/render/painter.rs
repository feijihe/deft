use crate::base::Rect;
use crate::border::tiny_path_to_skia_path;
use crate::canvas_util::CanvasHelper;
use crate::paint::{DrawLayer, InvalidRects, LayerState, Painter, RenderLayerKey};
use crate::render::paint_object::{ElementPO, LayerPO};
use crate::{show_focus_hint, show_layer_hint, show_repaint_area};
use skia_safe::{Canvas, ClipOp, Color, FilterMode, Matrix, Paint, PaintStyle, SamplingOptions};
use skia_window::context::RenderContext;
use std::collections::HashMap;
use std::mem;

pub struct ElementPainter {
    scale: f32,
    viewport: Rect,
    layer_state_map: HashMap<RenderLayerKey, LayerState>,
    layer_cache_enabled: bool,
}

impl ElementPainter {
    fn new() -> Self {
        Self {
            scale: 1.0,
            viewport: Rect::new_empty(),
            layer_state_map: HashMap::new(),
            layer_cache_enabled: false,
        }
    }

    pub fn take(ctx: &mut RenderContext) -> Self {
        ctx.user_context
            .take::<Self>()
            .unwrap_or_else(|| Self::new())
    }
    pub fn put(self, ctx: &mut RenderContext) {
        ctx.user_context.set(self);
    }

    pub fn update_viewport(&mut self, scale: f32, viewport: Rect) {
        self.scale = scale;
        self.viewport = viewport;
    }

    pub fn set_layer_cache(&mut self, layer_cache_enabled: bool) {
        self.layer_cache_enabled = layer_cache_enabled;
        if !layer_cache_enabled {
            self.layer_state_map.clear();
        }
    }

    pub fn draw_root(
        &mut self,
        painter: &Painter,
        root: &mut LayerPO,
        context: &mut RenderContext,
    ) {
        let mut state = mem::take(&mut self.layer_state_map);
        self.draw_layer(painter, context, root, &mut state, true);
    }

    fn draw_element_object_recurse(
        &mut self,
        painter: &Painter,
        epo: &mut ElementPO,
        context: &mut RenderContext,
    ) {
        let canvas = painter.canvas;
        // debug!("Painting {}", epo.element_id);
        //TODO optimize
        if !epo.need_paint && epo.children.is_empty() {
            return;
        }
        canvas.save();
        canvas.translate(epo.coord);
        canvas.clip_path(
            &tiny_path_to_skia_path(&epo.border_box_path),
            ClipOp::Intersect,
            false,
        );
        if epo.need_paint {
            self.draw_element_paint_object(painter, epo);
        }
        for e in &mut epo.children {
            self.draw_element_object_recurse(painter, e, context);
        }
        canvas.restore();
    }

    fn submit_layer(
        &mut self,
        painter: &Painter,
        _context: &mut RenderContext,
        lpo: &mut LayerPO,
        layer: &mut LayerState,
    ) {
        let img = match &mut layer.layer {
            DrawLayer::Root => return,
            DrawLayer::Sublayer(sl) => sl.as_image(),
        };
        let canvas = painter.canvas;
        canvas.save();
        canvas.translate((layer.surface_bounds.x, layer.surface_bounds.y));
        canvas.scale((1.0 / self.scale, 1.0 / self.scale));
        let mut options = SamplingOptions::default();
        //TODO use Nearest?
        options.filter = FilterMode::Linear;
        canvas.draw_image_with_sampling_options(img, (0.0, 0.0), options, None);
        canvas.restore();

        if show_repaint_area() {
            canvas.save();
            canvas.scale((self.scale, self.scale));
            let path = layer.invalid_rects.to_path();
            if !path.is_empty() {
                let mut paint = Paint::default();
                paint.set_style(PaintStyle::Stroke);
                paint.set_color(Color::from_rgb(200, 0, 0));
                canvas.draw_path(&path, &paint);
            }
            canvas.restore();
        }
        if show_layer_hint() {
            Self::paint_hit_rect(canvas, lpo.width, lpo.height);
        }
    }

    fn paint_hit_rect(canvas: &Canvas, width: f32, height: f32) {
        let rect = Rect::new(0.5, 0.5, width - 1.0, height - 1.0);
        let mut paint = Paint::default();
        paint.set_style(PaintStyle::Stroke);
        paint.set_color(Color::RED);
        canvas.draw_rect(&rect.to_skia_rect(), &paint);
    }

    pub fn draw_layer(
        &mut self,
        painter: &Painter,
        context: &mut RenderContext,
        layer: &mut LayerPO,
        layer_state_map: &mut HashMap<RenderLayerKey, LayerState>,
        is_root: bool,
    ) {
        let root_canvas = painter.canvas;
        let scale = self.scale;
        let surface_width = (layer.surface_bounds.width() * scale) as usize;
        let surface_height = (layer.surface_bounds.height() * scale) as usize;
        if surface_width <= 0 || surface_height <= 0 {
            return;
        }
        {
            let mut graphic_layer = if is_root {
                LayerState {
                    layer: DrawLayer::Root,
                    surface_width,
                    surface_height,
                    total_matrix: Matrix::default(),
                    invalid_rects: InvalidRects::default(),
                    surface_bounds: layer.surface_bounds,
                    matrix: Matrix::default(),
                }
            } else {
                self.get_graphic_layer(
                    context,
                    layer,
                    layer_state_map,
                    surface_width,
                    surface_height,
                    scale,
                )
            };
            graphic_layer.total_matrix = layer.total_matrix.clone();
            graphic_layer.matrix = layer.matrix.clone();
            let layer_canvas = match &mut graphic_layer.layer {
                DrawLayer::Root => painter.canvas,
                DrawLayer::Sublayer(sl) => sl.canvas(),
            };
            layer_canvas.save();

            layer_canvas.translate((
                -graphic_layer.surface_bounds.x,
                -graphic_layer.surface_bounds.y,
            ));
            if !layer.invalid_rects.is_empty() {
                layer_canvas.clip_path(&layer.invalid_rects.to_path(), ClipOp::Intersect, false);
                layer_canvas.clip_rect(
                    &Rect::from_xywh(0.0, 0.0, layer.width, layer.height).to_skia_rect(),
                    ClipOp::Intersect,
                    false,
                );
                layer_canvas.clear(Color::TRANSPARENT);
            }
            let layer_painter = Painter::new(layer_canvas, painter.context.clone());
            for e in &mut layer.elements {
                self.draw_element_object_recurse(&layer_painter, e, context);
            }
            layer_canvas.restore();
            context.flush();

            root_canvas.save();
            let old_total_matrix = root_canvas.local_to_device();
            root_canvas.concat(&layer.total_matrix);
            if let Some(clip_rect) = &layer.clip_rect {
                root_canvas.clip_rect(&clip_rect.to_skia_rect(), ClipOp::Intersect, false);
            } else {
                //TODO support overflow
                let rect = Rect::from_xywh(0.0, 0.0, layer.width, layer.height);
                root_canvas.clip_rect(&rect.to_skia_rect(), ClipOp::Intersect, false);
            }
            self.submit_layer(painter, context, layer, &mut graphic_layer);
            context.flush();
            if self.layer_cache_enabled {
                self.layer_state_map
                    .insert(layer.key.clone(), graphic_layer);
            }
            root_canvas.set_matrix(&old_total_matrix);
        }

        for l in &mut layer.layers {
            self.draw_layer(painter, context, l, layer_state_map, false);
        }
        root_canvas.restore();
    }

    fn get_graphic_layer(
        &mut self,
        context: &mut RenderContext,
        layer: &mut LayerPO,
        layer_state_map: &mut HashMap<RenderLayerKey, LayerState>,
        surface_width: usize,
        surface_height: usize,
        scale: f32,
    ) -> LayerState {
        if let Some(mut ogl_state) = layer_state_map.remove(&layer.key) {
            let sublayer = match &mut ogl_state.layer {
                DrawLayer::Sublayer(sl) => sl,
                DrawLayer::Root => unreachable!(),
            };
            if ogl_state.surface_width != surface_width
                || ogl_state.surface_height != surface_height
            {
                None
            } else {
                //TODO fix scroll delta
                let scroll_delta_x = layer.surface_bounds.x - ogl_state.surface_bounds.x;
                let scroll_delta_y = layer.surface_bounds.y - ogl_state.surface_bounds.y;
                if scroll_delta_x != 0.0 || scroll_delta_y != 0.0 {
                    let mut temp_gl = context.create_layer(surface_width, surface_height).unwrap();
                    temp_gl.canvas().session(|canvas| {
                        // canvas.clip_rect(&Rect::new(0.0, 0.0, layer.width * scale, layer.height * scale), ClipOp::Intersect, false);
                        canvas.clear(Color::TRANSPARENT);
                        canvas.draw_image(
                            &sublayer.as_image(),
                            (-scroll_delta_x * scale, -scroll_delta_y * scale),
                            None,
                        );
                    });
                    context.flush();

                    sublayer.canvas().session(|canvas| {
                        canvas.clear(Color::TRANSPARENT);
                        // canvas.clip_rect(&Rect::from_xywh(0.0, 0.0, layer.width, layer.height), ClipOp::Intersect, false);
                        canvas.scale((1.0 / scale, 1.0 / scale));
                        canvas.draw_image(&temp_gl.as_image(), (0.0, 0.0), None);
                    });
                    context.flush();
                }
                ogl_state.surface_bounds = layer.surface_bounds;
                Some(ogl_state)
            }
        } else {
            None
        }
        .unwrap_or_else(|| {
            let mut gl = context.create_layer(surface_width, surface_height).unwrap();
            gl.canvas().scale((scale, scale));
            LayerState {
                layer: DrawLayer::Sublayer(gl),
                surface_width,
                surface_height,
                total_matrix: Matrix::default(),
                invalid_rects: InvalidRects::default(),
                surface_bounds: layer.surface_bounds,
                matrix: Matrix::default(),
            }
        })
    }

    fn draw_element_paint_object(&mut self, painter: &Painter, node: &mut ElementPO) {
        let width = node.width;
        let height = node.height;
        //TODO fix clip
        // node.clip_chain.apply(canvas);
        // canvas.concat(&node.total_matrix);
        // node.clip_path.apply(canvas);

        painter.canvas.session(move |canvas| {
            // draw background and border
            node.draw_background(&canvas);
            node.draw_border(&canvas);

            // draw padding box and content box
            canvas.save();
            if width > 0.0 && height > 0.0 {
                let (border_top_width, _, _, border_left_width) = node.border_width;
                // let (padding_top, _, _, padding_left) = element.get_padding();
                // draw content box
                canvas.translate((border_left_width, border_top_width));
                // let paint_info = some_or_return!(&mut node.paint_info);
                if let Some(render_fn) = node.render_fn.take() {
                    render_fn.run(painter);
                }
            }
            canvas.restore();
            if show_focus_hint() && node.focused {
                node.draw_hit_rect(canvas);
            }
        });
    }
}
