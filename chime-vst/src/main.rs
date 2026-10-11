use chime_vst::ChimeVst;
use nih_plug::prelude::*;

fn main() {
    nih_export_standalone::<ChimeVst>();
}
