//! A small egui editor: model choice, mix, bypass and a status line.

use crate::{ModelChoice, Shared, Status, VfParams};
use nih_plug::prelude::*;
use nih_plug_egui::egui::{self, Color32, RichText};
use nih_plug_egui::{create_egui_editor, widgets, EguiState};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const WIDTH: u32 = 380;
const HEIGHT: u32 = 230;

pub(crate) fn default_state() -> Arc<EguiState> {
    EguiState::from_size(WIDTH, HEIGHT)
}

pub(crate) fn create(params: Arc<VfParams>, shared: Arc<Shared>) -> Option<Box<dyn Editor>> {
    let cache_dir = vf_core::cache_dir().map(|d| d.display().to_string()).unwrap_or_default();
    create_egui_editor(
        params.editor_state.clone(),
        (),
        |_, _| {},
        move |ctx, setter, _| {
            // Status changes come from other threads, so poll a few times a second.
            ctx.request_repaint_after(Duration::from_millis(250));
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Chime Voice Focus");
                ui.add_space(8.0);

                egui::Grid::new("controls").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                    ui.label("Model");
                    model_combo(ui, &params, setter);
                    ui.end_row();

                    ui.label("Mix");
                    ui.add(widgets::ParamSlider::for_param(&params.mix, setter).with_width(200.0));
                    ui.end_row();

                    ui.label("Bypass");
                    let mut bypass = params.bypass.value();
                    if ui.checkbox(&mut bypass, "").changed() {
                        setter.begin_set_parameter(&params.bypass);
                        setter.set_parameter(&params.bypass, bypass);
                        setter.end_set_parameter(&params.bypass);
                    }
                    ui.end_row();
                });

                ui.add_space(6.0);
                ui.separator();
                let status = shared.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
                status_line(ui, &status, params.bypass.value(), shared.wet_active.load(Ordering::Relaxed));

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(RichText::new(format!("Models: {cache_dir}")).small().weak());
                });
            });
        },
    )
}

fn model_combo(ui: &mut egui::Ui, params: &VfParams, setter: &ParamSetter) {
    let current = params.model.value();
    egui::ComboBox::from_id_salt("model")
        .width(200.0)
        .selected_text(ModelChoice::variants()[current.to_index()])
        .show_ui(ui, |ui| {
            for (i, name) in ModelChoice::variants().iter().enumerate() {
                let choice = ModelChoice::from_index(i);
                if ui.selectable_label(choice == current, *name).clicked() && choice != current {
                    setter.begin_set_parameter(&params.model);
                    setter.set_parameter(&params.model, choice);
                    setter.end_set_parameter(&params.model);
                }
            }
        });
}

fn status_line(ui: &mut egui::Ui, status: &Status, bypass: bool, wet_active: bool) {
    const GREEN: Color32 = Color32::from_rgb(110, 200, 120);
    const AMBER: Color32 = Color32::from_rgb(230, 180, 80);
    const RED: Color32 = Color32::from_rgb(230, 100, 100);

    match status {
        Status::Loading(variant) => {
            ui.colored_label(AMBER, format!("Loading {}…", variant.name()));
            ui.label(RichText::new("The first use of each model downloads it from Amazon's CDN.").small());
        }
        Status::Failed { variant, error } => {
            ui.colored_label(RED, format!("Couldn't load {}. Audio passes through unprocessed.", variant.name()));
            ui.label(RichText::new(error).small());
        }
        Status::Ready { variant, latency, sample_rate } => {
            if bypass {
                ui.label(RichText::new("Bypassed").weak());
            } else if wet_active {
                ui.colored_label(GREEN, format!("Active: {}", variant.name()));
            } else {
                ui.colored_label(AMBER, format!("Starting {}…", variant.name()));
            }
            ui.label(
                RichText::new(format!(
                    "Latency {:.1} ms ({latency} samples at {sample_rate} Hz)",
                    *latency as f64 * 1000.0 / *sample_rate as f64
                ))
                .small(),
            );
        }
    }
}
