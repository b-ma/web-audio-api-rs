use std::f32::consts::PI;
use std::fmt::Debug;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::alloc::ChannelData;
use crate::buffer::{ChannelConfig, ChannelConfigOptions};
use crate::context::{AsBaseAudioContext, AudioContextRegistration, AudioParamId};
use crate::control::Scheduler;
use crate::param::{AudioParam, AudioParamOptions};
use crate::process::{AudioParamValues, AudioProcessor};
use crate::SampleRate;

use crossbeam_channel::{self, Receiver, Sender};
use lazy_static::lazy_static;

use super::{AudioNode, AudioScheduledSourceNode};

const TABLE_LENGTH_USIZE: usize = 2048;
const TABLE_LENGTH_F32: f32 = TABLE_LENGTH_USIZE as f32;

// Compute one period sine wavetable of size TABLE_LENGTH
lazy_static! {
    static ref SINETABLE: Vec<f32> = {
        let table: Vec<f32> = (0..TABLE_LENGTH_USIZE)
            .map(|x| ((x as f32) * 2.0 * PI * (1. / (TABLE_LENGTH_F32))).sin())
            .collect();
        table
    };
}

/// Options for constructing a periodic wave
pub struct PeriodicWaveOptions {
    /// The real parameter represents an array of cosine terms of Fourrier series.
    ///
    /// The first element (index 0) represents the DC-offset.
    /// This offset has to be given but will not be taken into account
    /// to build the custom periodic waveform.
    ///
    /// The following elements (index 1 and more) represent the fundamental and harmonics of the periodic waveform.
    pub real: Vec<f32>,
    /// The imag parameter represents an array of sine terms of Fourrier series.
    ///
    /// The first element (index 0) will not be taken into account
    /// to build the custom periodic waveform.
    ///
    /// The following elements (index 1 and more) represent the fundamental and harmonics of the periodic waveform.
    pub imag: Vec<f32>,
    /// By default PeriodicWave is build with normalization enabled (disable_normalization = false).
    /// In this case, a peak normalization is applied to the given custom periodic waveform.
    ///
    /// If disable_normalization is enabled (disable_normalization = true), the normalization is
    /// defined by the periodic waveform characteristics (img, and real fields).
    pub disable_normalization: bool,
}

/// PeriodicWave is a setup struct required to build
/// custom periodic waveform oscillator type.
#[derive(Debug, Clone)]
pub struct PeriodicWave {
    /// The real parameter represents an array of cosine terms of Fourrier series.
    ///
    /// The first element (index 0) represents the DC-offset.
    /// This offset has to be given but will not be taken into account
    /// to build the custom periodic waveform.
    ///
    /// The following elements (index 1 and more) represent the fundamental and harmonics of the periodic waveform.
    real: Vec<f32>,
    /// The imag parameter represents an array of sine terms of Fourrier series.
    ///
    /// The first element (index 0) will not be taken into account
    /// to build the custom periodic waveform.
    ///
    /// The following elements (index 1 and more) represent the fundamental and harmonics of the periodic waveform.
    imag: Vec<f32>,
    /// By default PeriodicWave is build with normalization enabled (disable_normalization = false).
    /// In this case, a peak normalization is applied to the given custom periodic waveform.
    ///
    /// If disable_normalization is enabled (disable_normalization = true), the normalization is
    /// defined by the periodic waveform characteristics (img, and real fields).
    disable_normalization: bool,
}

impl PeriodicWave {
    /// Returns a PeriodicWave
    ///
    /// # Arguments
    ///
    /// * `real` - The real parameter represents an array of cosine terms of Fourrier series.
    /// * `imag` - The imag parameter represents an array of sine terms of Fourrier series.
    /// * `constraints` - The constraints parameter specifies the normalization mode of the PeriodicWave
    ///
    /// # Example
    ///
    /// ```no_run
    ///    use web_audio_api::context::{AudioContext, AsBaseAudioContext};
    ///    use web_audio_api::node::{PeriodicWave, PeriodicWaveOptions};
    ///
    ///    let context = AudioContext::new();
    ///
    ///    let options = PeriodicWaveOptions {
    ///    real: vec![0.,1.,1.],
    ///    imag: vec![0.,0.,0.],
    ///    disable_normalization: false,
    ///    };
    ///
    ///    let periodic_wave = PeriodicWave::new(&context, Some(options));
    /// ```
    ///
    pub fn new<C: AsBaseAudioContext>(_context: &C, options: Option<PeriodicWaveOptions>) -> Self {
        if let Some(PeriodicWaveOptions {
            real,
            imag,
            disable_normalization,
        }) = options
        {
            // Todo: assertions not complete. Missing assertions when
            // only real field is specified, anly imag field is specified,
            // and neither real or imag is specified
            assert!(
                real.len() >= 2,
                "RangeError: Real field length should be at least 2"
            );
            assert!(
                imag.len() >= 2,
                "RangeError: Imag field length should be at least 2",
            );
            assert!(
                real.len() == imag.len(),
                "RangeError: Imag and real field length should be equal"
            );
            Self {
                real,
                imag,
                disable_normalization,
            }
        } else {
            Self {
                real: vec![0., 0.],
                imag: vec![0., 1.],
                disable_normalization: false,
            }
        }
    }
}

/// Options for constructing an OscillatorNode
#[derive(Debug)]
pub struct OscillatorOptions {
    pub type_: OscillatorType,
    pub frequency: f32,
    pub detune: f32,
    pub channel_config: ChannelConfigOptions,
    pub periodic_wave: Option<PeriodicWave>,
}

impl Default for OscillatorOptions {
    fn default() -> Self {
        Self {
            type_: OscillatorType::default(),
            frequency: 440.,
            detune: 0.,
            channel_config: ChannelConfigOptions::default(),
            periodic_wave: None,
        }
    }
}

/// Waveform of an oscillator
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum OscillatorType {
    Sine,
    Square,
    Sawtooth,
    Triangle,
    Custom,
}

impl Default for OscillatorType {
    fn default() -> Self {
        OscillatorType::Sine
    }
}

impl From<u32> for OscillatorType {
    fn from(i: u32) -> Self {
        use OscillatorType::*;

        match i {
            0 => Sine,
            1 => Square,
            2 => Sawtooth,
            3 => Triangle,
            4 => Custom,
            _ => unreachable!(),
        }
    }
}

/// Message types used to communicate between [OscillatorNode] and [OscillatorRenderer]
enum OscMsg {
    PeriodicWaveMsg {
        computed_freq: f32,
        wavetable: Vec<f32>,
        norm_factor: f32,
        disable_normalization: bool,
    },
}

/// Audio source generating a periodic waveform
pub struct OscillatorNode {
    registration: AudioContextRegistration,
    channel_config: ChannelConfig,
    sample_rate: f32,
    frequency: AudioParam,
    detune: AudioParam,
    type_: Arc<AtomicU32>,
    scheduler: Scheduler,
    sender: Sender<OscMsg>,
}

impl AudioScheduledSourceNode for OscillatorNode {
    fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }
}

impl AudioNode for OscillatorNode {
    fn registration(&self) -> &AudioContextRegistration {
        &self.registration
    }

    fn channel_config_raw(&self) -> &ChannelConfig {
        &self.channel_config
    }

    /// OscillatorNode is a source node. A source node is by definition with no input
    fn number_of_inputs(&self) -> u32 {
        0
    }

    /// OscillatorNode is a mono source node.
    fn number_of_outputs(&self) -> u32 {
        1
    }
}

impl OscillatorNode {
    /// Returns an OscillatorNode
    ///
    /// # Arguments:
    ///
    /// * `context` - The AudioContext
    /// * `options` - The Oscillatoroptions
    pub fn new<C: AsBaseAudioContext>(context: &C, options: OscillatorOptions) -> Self {
        context.base().register(move |registration| {
            let sample_rate = context.base().sample_rate().0 as f32;
            let nyquist = sample_rate / 2.;
            let default_freq = 440.;
            let default_det = 0.;

            // frequency audio parameter
            let freq_param_opts = AudioParamOptions {
                min_value: -nyquist,
                max_value: nyquist,
                default_value: default_freq,
                automation_rate: crate::param::AutomationRate::A,
            };
            let (f_param, f_proc) = context
                .base()
                .create_audio_param(freq_param_opts, registration.id());
            f_param.set_value(options.frequency);

            // detune audio parameter
            let det_param_opts = AudioParamOptions {
                min_value: -153600.,
                max_value: 153600.,
                default_value: default_det,
                automation_rate: crate::param::AutomationRate::A,
            };
            let (det_param, det_proc) = context
                .base()
                .create_audio_param(det_param_opts, registration.id());
            det_param.set_value(options.detune);

            let type_ = Arc::new(AtomicU32::new(options.type_ as u32));
            let scheduler = Scheduler::new();

            let (sender, receiver) = crossbeam_channel::bounded(0);

            let computed_freq = default_freq * 2f32.powf(default_det / 1200.);

            let config = OscRendererConfig {
                type_: type_.clone(),
                frequency: f_proc,
                detune: det_proc,
                scheduler: scheduler.clone(),
                receiver,
                computed_freq,
                sample_rate,
                periodic_wave: options.periodic_wave.clone(),
            };
            let render = OscillatorRenderer::new(config);
            let node = OscillatorNode {
                registration,
                channel_config: options.channel_config.into(),
                sample_rate,
                frequency: f_param,
                detune: det_param,
                type_,
                scheduler,
                sender,
            };

            (node, Box::new(render))
        })
    }

    /// Returns the frequency audio parameter
    /// The oscillator frequency is calculated as follow:
    /// frequency * 2^(detune/1200)
    pub fn frequency(&self) -> &AudioParam {
        &self.frequency
    }

    /// Returns the detune audio parameter. detune unity is cents.
    /// The oscillator frequency is calculated as follow:
    /// frequency * 2^(detune/1200)
    pub fn detune(&self) -> &AudioParam {
        &self.detune
    }

    /// Returns the computedOscFrequency which is the oscillator frequency.
    fn computed_freq(&self) -> f32 {
        let frequency = self.frequency().value();
        let detune = self.detune().value();
        frequency * 2f32.powf(detune / 1200.)
    }

    /// Returns the oscillator type
    pub fn type_(&self) -> OscillatorType {
        self.type_.load(Ordering::SeqCst).into()
    }

    /// set the oscillator type
    ///
    /// # Arguments
    ///
    /// * `type_` - oscillator type (sine,square,triangle,sawtooth, and custom)
    pub fn set_type(&self, type_: OscillatorType) {
        self.type_.store(type_ as u32, Ordering::SeqCst);
    }

    /// set the oscillator type to custom and generate
    /// a perdioc waveform following the PeriodicWave characteristics
    pub fn set_periodic_wave(&mut self, periodic_wave: PeriodicWave) {
        self.set_type(OscillatorType::Custom);

        let PeriodicWave {
            real,
            imag,
            disable_normalization,
        } = periodic_wave;

        let mut cplxs = Vec::with_capacity(real.len());
        let mut norms = Vec::with_capacity(real.len());
        let mut phases = Vec::with_capacity(real.len());
        let mut incr_phases = Vec::with_capacity(real.len());
        let mut interpol_ratios = Vec::with_capacity(real.len());

        // update cplxs
        for cplx in real.into_iter().zip(imag) {
            cplxs.push(cplx);
        }

        let computed_freq = self.computed_freq();

        for (idx, (real, img)) in cplxs.iter().enumerate() {
            // update norms
            norms.push((f32::powi(*real, 2i32) + f32::powi(*img, 2i32)).sqrt());

            // update phases
            let phase = f32::atan2(*img, *real);
            if phase < 0. {
                phases.push((phase + 2. * PI) * (TABLE_LENGTH_F32 / (2.0 * PI)));
            } else {
                phases.push(phase * (TABLE_LENGTH_F32 / 2.0 * PI));
            }

            // update incr_phases
            incr_phases.push(TABLE_LENGTH_F32 * idx as f32 * (computed_freq / self.sample_rate));
        }

        // update interpol_ratios
        for incr_phase in &incr_phases {
            interpol_ratios.push((incr_phase - incr_phase.round()).abs());
        }

        // generate the wavetable following periodic wave characteristics
        let wavetable =
            self.generate_wavetable(&norms, &mut phases, &incr_phases, &interpol_ratios);

        // update norm_factor
        let norm_factor = self.norm_factor(&wavetable);

        self.sender
            .send(OscMsg::PeriodicWaveMsg {
                computed_freq,
                wavetable,
                norm_factor,
                disable_normalization,
            })
            .expect("Sending periodic wave to the node renderer failed");
    }

    /// Generate the wavetable
    ///
    /// # Arguments
    ///
    /// * `norms` - the norm of each harmonics
    /// * `phases` - the phase of each harmonics
    /// * `incr_phases` - the phase to increment of each harmonics
    /// * `interpol_ratios` - the interpolation ratio of each harmonics used by linear interpolation
    fn generate_wavetable(
        &self,
        norms: &[f32],
        phases: &mut [f32],
        incr_phases: &[f32],
        interpol_ratios: &[f32],
    ) -> Vec<f32> {
        let mut buffer = Vec::new();

        while phases[1] <= TABLE_LENGTH_F32 {
            let mut sample = 0.0;
            for i in 1..phases.len() {
                let gain = norms[i];
                let phase = phases[i];
                let incr_phase = incr_phases[i];
                let mu = interpol_ratios[i];
                let idx = (phase + incr_phase) as usize;
                let inf_idx = idx % TABLE_LENGTH_USIZE;
                let sup_idx = (idx + 1) % TABLE_LENGTH_USIZE;
                // Linear interpolation
                sample += (SINETABLE[inf_idx] * (1. - mu) + SINETABLE[sup_idx] * mu) * gain;
                phases[i] = phase + incr_phase;
            }

            buffer.push(sample);
        }

        buffer
    }

    /// Compute the normalization factor
    ///
    /// The normalization factor is applied as a gain to the periodic wave
    /// to normalize the signal peak amplitude in the interval [-1.0,1.0].
    ///
    /// # Arguments
    ///
    /// * `buffer` - the wavetable generated from periodic wave charateristics
    fn norm_factor(&self, buffer: &[f32]) -> f32 {
        1. / buffer
            .iter()
            .copied()
            .reduce(f32::max)
            .expect("Maximum value not found")
    }
}

/// States relative to Sine OscillatorType
struct SineState {
    interpol_ratio: f32,
    first: bool,
}

/// States relative to Triangle OscillatorType
struct TriangleState {
    last_output: f32,
}

/// States relative to Custom OscillatorType
struct PeriodicState {
    incr_phases: Vec<f32>,
    interpol_ratios: Vec<f32>,
    norm_factor: Option<f32>,
    disable_normalization: bool,
    wavetable: WavetableState,
}

/// States required to generate the periodic wavetable
struct WavetableState {
    buffer: Vec<f32>,
    phase: f32,
    incr_phase: f32,
    ref_freq: f32,
}

/// Rendering component of the oscillator node
struct OscillatorRenderer {
    type_: Arc<AtomicU32>,
    frequency: AudioParamId,
    detune: AudioParamId,
    scheduler: Scheduler,
    receiver: Receiver<OscMsg>,
    computed_freq: f32,
    sample_rate: f32,
    phase: f32,
    incr_phase: f32,
    sine: SineState,
    triangle: TriangleState,
    periodic: PeriodicState,
}

impl AudioProcessor for OscillatorRenderer {
    fn process(
        &mut self,
        _inputs: &[crate::alloc::AudioBuffer],
        outputs: &mut [crate::alloc::AudioBuffer],
        params: AudioParamValues,
        timestamp: f64,
        _sample_rate: SampleRate,
    ) {
        // single output node
        let output = &mut outputs[0];

        // re-use previous buffer
        output.force_mono();

        // todo, sub-quantum start/stop
        if !self.scheduler.is_active(timestamp) {
            output.make_silent();
            return;
        }

        let freq_values = params.get(&self.frequency);

        let det_values = params.get(&self.detune);

        let mut computed_freqs: [f32; 128] = [0.; 128];

        if det_values
            .windows(2)
            .all(|w| (w[0] - w[1]).abs() < 0.000001)
        {
            let d = 2f32.powf(det_values[0] / 1200.);
            for (i, f) in freq_values.iter().enumerate() {
                computed_freqs[i] = f * d
            }
        } else {
            for (i, (f, d)) in freq_values.iter().zip(det_values).enumerate() {
                computed_freqs[i] = f * 2f32.powf(d / 1200.);
            }
        }

        let type_ = self.type_.load(Ordering::SeqCst).into();

        let buffer = output.channel_data_mut(0);

        // check if any message was send from the control thread
        if let Ok(msg) = self.receiver.try_recv() {
            match msg {
                OscMsg::PeriodicWaveMsg {
                    computed_freq,
                    wavetable,
                    norm_factor,
                    disable_normalization,
                } => self.set_periodic_wave(
                    computed_freq,
                    wavetable,
                    norm_factor,
                    disable_normalization,
                ),
            }
        }

        self.generate_output(type_, buffer, &computed_freqs[..]);
    }

    fn tail_time(&self) -> bool {
        true
    }
}

struct OscRendererConfig {
    type_: Arc<AtomicU32>,
    frequency: AudioParamId,
    detune: AudioParamId,
    scheduler: Scheduler,
    receiver: Receiver<OscMsg>,
    computed_freq: f32,
    sample_rate: f32,
    periodic_wave: Option<PeriodicWave>,
}

impl OscillatorRenderer {
    fn new(config: OscRendererConfig) -> Self {
        let OscRendererConfig {
            type_,
            frequency,
            detune,
            scheduler,
            receiver,
            computed_freq,
            sample_rate,
            periodic_wave,
        } = config;
        let incr_phase = computed_freq / sample_rate;
        let interpol_ratio = (incr_phase - incr_phase.floor()) * TABLE_LENGTH_F32;

        let PeriodicWave {
            real,
            imag,
            disable_normalization,
        } = if let Some(p_w) = periodic_wave {
            p_w
        } else {
            PeriodicWave {
                real: vec![0., 1.],
                imag: vec![0., 0.],
                disable_normalization: false,
            }
        };

        let cplxs: Vec<(f32, f32)> = real.iter().zip(&imag).map(|(&r, &i)| (r, i)).collect();

        let norms: Vec<f32> = cplxs
            .iter()
            .map(|(r, i)| (f32::powi(*r, 2i32) + f32::powi(*i, 2i32)).sqrt())
            .collect();

        let mut phases: Vec<f32> = cplxs
            .iter()
            .map(|(r, i)| {
                let phase = f32::atan2(*i, *r);
                if phase < 0. {
                    (phase + 2. * PI) * (TABLE_LENGTH_F32 / (2.0 * PI))
                } else {
                    phase * (TABLE_LENGTH_F32 / 2.0 * PI)
                }
            })
            .collect();

        let incr_phases: Vec<f32> = cplxs
            .iter()
            .enumerate()
            .map(|(idx, _)| TABLE_LENGTH_F32 * idx as f32 * (computed_freq / sample_rate))
            .collect();

        let interpol_ratios: Vec<f32> = incr_phases
            .iter()
            .map(|incr_phase| incr_phase - incr_phase.floor())
            .collect();

        let mut periodic_wavetable = Vec::with_capacity(2048);

        Self::generate_wavetable(
            &norms,
            &mut phases,
            &incr_phases,
            &interpol_ratios,
            &mut periodic_wavetable,
        );

        let norm_factor = if !disable_normalization {
            let norm_factor = Self::norm_factor(&periodic_wavetable);
            Some(norm_factor)
        } else {
            None
        };

        Self {
            type_,
            frequency,
            detune,
            scheduler,
            receiver,
            computed_freq,
            sample_rate,
            phase: 0.0,
            incr_phase,
            sine: SineState {
                interpol_ratio,
                first: true,
            },
            triangle: TriangleState { last_output: 0.0 },
            periodic: PeriodicState {
                incr_phases,
                interpol_ratios,
                norm_factor,
                disable_normalization,
                wavetable: WavetableState {
                    buffer: periodic_wavetable,
                    phase: 0.,
                    incr_phase: 1.,
                    ref_freq: computed_freq,
                },
            },
        }
    }

    /// set periodic states
    ///
    /// # Arguments
    ///
    /// * `ref_freq` - the computedOscFrequency used to build the wavetable
    /// * `wavetable` - wavetable following periodic wave characteristics
    /// * `norm_factor` - normalization factor applied when disable_normalization is false
    /// * `disable_normalization` - disable normalization. If false, the peak amplitude signal is 1.0
    fn set_periodic_wave(
        &mut self,
        ref_freq: f32,
        wavetable: Vec<f32>,
        norm_factor: f32,
        disable_normalization: bool,
    ) {
        self.periodic.wavetable.ref_freq = ref_freq;
        self.periodic.wavetable.buffer = wavetable;
        self.periodic.norm_factor = Some(norm_factor);
        self.periodic.disable_normalization = disable_normalization;
    }

    /// Compute params at each audio sample for the following oscillator type:
    /// * sine
    /// * sawtooth
    /// * triangle
    /// * and square
    fn arate_calc_params(&mut self, type_: OscillatorType, computed_freq: f32) {
        // No need to compute if frequency has not changed
        if type_ == OscillatorType::Sine {
            if self.sine.first {
                self.sine.first = false;
                self.incr_phase = computed_freq / self.sample_rate * TABLE_LENGTH_F32;
            }
            if (self.computed_freq - computed_freq).abs() < 0.01 {
                return;
            }
            self.computed_freq = computed_freq;
            self.incr_phase = computed_freq / self.sample_rate * TABLE_LENGTH_F32;
        }
        if (self.computed_freq - computed_freq).abs() < 0.01 {
            return;
        }
        self.computed_freq = computed_freq;
        self.incr_phase = computed_freq / self.sample_rate;
    }

    /// Compute params at each audio sample for the custom oscillator type
    fn arate_calc_periodic_params(&mut self, new_comp_freq: f32) {
        // No need to compute if frequency has not changed
        if (self.computed_freq - new_comp_freq).abs() < 0.01 {
            return;
        }

        for incr_phase in &mut self.periodic.incr_phases {
            *incr_phase *= new_comp_freq / self.computed_freq;
        }

        for (r, incr_ph) in self
            .periodic
            .interpol_ratios
            .iter_mut()
            .zip(self.periodic.incr_phases.iter())
        {
            *r = incr_ph - incr_ph.floor();
        }

        self.periodic.wavetable.incr_phase = new_comp_freq / self.periodic.wavetable.ref_freq;
        self.computed_freq = new_comp_freq;
    }

    /// generate the audio data according to the oscillator type and frequency parameters
    /// buffer is filled with the generated audio data.
    ///
    /// # Arguments
    ///
    /// * `type_` - oscillator type (sine,sawtooth,triangle,square, or custom)
    /// * `buffer` - audio output buffer
    /// * `freq_values` - frequencies at which each sample should be generated
    fn generate_output(
        &mut self,
        type_: OscillatorType,
        buffer: &mut ChannelData,
        freq_values: &[f32],
    ) {
        match type_ {
            OscillatorType::Sine => self.generate_sine(type_, buffer, freq_values),
            OscillatorType::Square => self.generate_square(type_, buffer, freq_values),
            OscillatorType::Sawtooth => self.generate_sawtooth(type_, buffer, freq_values),
            OscillatorType::Triangle => self.generate_triangle(type_, buffer, freq_values),
            OscillatorType::Custom => self.generate_custom(buffer, freq_values),
        }
    }

    /// generate the audio data when oscillator is of type sine
    /// buffer is filled with the sine audio data.
    ///
    /// # Arguments
    ///
    /// * `type_` - oscillator type (sine,sawtooth,triangle,square, or custom)
    /// * `buffer` - audio output buffer
    /// * `freq_values` - frequencies at which each sample should be generated
    fn generate_sine(
        &mut self,
        type_: OscillatorType,
        buffer: &mut ChannelData,
        freq_values: &[f32],
    ) {
        for (o, &computed_freq) in buffer.iter_mut().zip(freq_values) {
            self.arate_calc_params(type_, computed_freq);
            let idx = self.phase as usize;
            let inf_idx = idx % TABLE_LENGTH_USIZE;
            let sup_idx = (idx + 1) % TABLE_LENGTH_USIZE;

            // Linear interpolation
            *o = SINETABLE[inf_idx] * (1. - self.sine.interpol_ratio)
                + SINETABLE[sup_idx] * self.sine.interpol_ratio;

            // Optimized float modulo op
            self.phase = if self.phase + self.incr_phase >= TABLE_LENGTH_F32 {
                (self.phase + self.incr_phase) - TABLE_LENGTH_F32
            } else {
                self.phase + self.incr_phase
            };
        }
    }

    /// generate the audio data when oscillator is of type sawtooth
    /// buffer is filled with the sawtooth audio data.
    ///
    /// # Arguments
    ///
    /// * `type_` - oscillator type (sine,sawtooth,triangle,square, or custom)
    /// * `buffer` - audio output buffer
    /// * `freq_values` - frequencies at which each sample should be generated
    fn generate_sawtooth(
        &mut self,
        type_: OscillatorType,
        buffer: &mut ChannelData,
        freq_values: &[f32],
    ) {
        for (o, &computed_freq) in buffer.iter_mut().zip(freq_values) {
            self.arate_calc_params(type_, computed_freq);
            let mut sample = (2.0 * self.phase) - 1.0;
            sample -= self.poly_blep(self.phase);

            // Optimized float modulo op
            self.phase += self.incr_phase;
            while self.phase >= 1. {
                self.phase -= 1.;
            }

            *o = sample;
        }
    }

    /// generate the audio data when oscillator is of type square
    /// buffer is filled with the square audio data.
    ///
    /// # Arguments
    ///
    /// * `type_` - oscillator type (sine,sawtooth,triangle,square, or custom)
    /// * `buffer` - audio output buffer
    /// * `freq_values` - frequencies at which each sample should be generated
    fn generate_square(
        &mut self,
        type_: OscillatorType,
        buffer: &mut ChannelData,
        freq_values: &[f32],
    ) {
        for (o, &computed_freq) in buffer.iter_mut().zip(freq_values) {
            self.arate_calc_params(type_, computed_freq);
            let mut sample = if self.phase <= 0.5 { 1.0 } else { -1.0 };

            sample += self.poly_blep(self.phase);

            // Optimized float modulo op
            let mut shift_phase = self.phase + 0.5;
            while shift_phase >= 1. {
                shift_phase -= 1.
            }
            sample -= self.poly_blep(shift_phase);

            // Optimized float modulo op
            self.phase += self.incr_phase;
            while self.phase >= 1. {
                self.phase -= 1.
            }
            *o = sample;
        }
    }

    /// generate the audio data when oscillator is of type triangle
    /// buffer is filled with the triangle audio data.
    ///
    /// # Arguments
    ///
    /// * `type_` - oscillator type (sine,sawtooth,triangle,square, or custom)
    /// * `buffer` - audio output buffer
    /// * `freq_values` - frequencies at which each sample should be generated
    fn generate_triangle(
        &mut self,
        type_: OscillatorType,
        buffer: &mut ChannelData,
        freq_values: &[f32],
    ) {
        for (o, &computed_freq) in buffer.iter_mut().zip(freq_values) {
            self.arate_calc_params(type_, computed_freq);
            let mut sample = if self.phase <= 0.5 { 1.0 } else { -1.0 };

            sample += self.poly_blep(self.phase);

            // Optimized float modulo op
            let mut shift_phase = self.phase + 0.5;
            while shift_phase >= 1. {
                shift_phase -= 1.
            }
            sample -= self.poly_blep(shift_phase);

            // Optimized float modulo op
            self.phase += self.incr_phase;
            while self.phase >= 1. {
                self.phase -= 1.
            }

            // Leaky integrator: y[n] = A * x[n] + (1 - A) * y[n-1]
            // Classic integration cannot be used due to float errors accumulation over execution time
            sample = self.incr_phase * sample + (1.0 - self.incr_phase) * self.triangle.last_output;
            self.triangle.last_output = sample;

            // Normalized amplitude into intervall [-1.0,1.0]
            *o = sample * 4.;
        }
    }

    /// generate the audio data when oscillator is of type custom
    /// buffer is filled with the periodic waveform audio data.
    ///
    /// # Arguments
    ///
    /// * `type_` - oscillator type (sine,sawtooth,triangle,square, or custom)
    /// * `buffer` - audio output buffer
    /// * `freq_values` - frequencies at which each sample should be generated
    fn generate_custom(&mut self, buffer: &mut ChannelData, freq_values: &[f32]) {
        for (o, &computed_freq) in buffer.iter_mut().zip(freq_values) {
            self.arate_calc_periodic_params(computed_freq);
            self.periodic.wavetable.phase = (self.periodic.wavetable.phase
                + self.periodic.wavetable.incr_phase)
                % self.periodic.wavetable.buffer.len() as f32;
            *o = self.periodic.wavetable.buffer[self.periodic.wavetable.phase as usize];
        }
    }

    /// Generate the wavetable
    ///
    /// # Arguments
    ///
    /// * `norms` - the norm of each harmonics
    /// * `phases` - the phase of each harmonics
    /// * `incr_phases` - the phase to increment of each harmonics
    /// * `interpol_ratios` - the interpolation ratio of each harmonics used by linear interpolatio
    /// * `buffer` - the buffer is filled with generated wavetable data (avoid allocation)
    fn generate_wavetable(
        norms: &[f32],
        phases: &mut [f32],
        incr_phases: &[f32],
        interpol_ratios: &[f32],
        buffer: &mut Vec<f32>,
    ) {
        buffer.clear();

        while phases[1] <= TABLE_LENGTH_F32 {
            let mut sample = 0.0;
            for i in 1..phases.len() {
                let gain = norms[i];
                let phase = phases[i];
                let incr_phase = incr_phases[i];
                let mu = interpol_ratios[i];
                let idx = (phase + incr_phase) as usize;
                let inf_idx = idx % TABLE_LENGTH_USIZE;
                let sup_idx = (idx + 1) % TABLE_LENGTH_USIZE;
                // Linear interpolation
                sample += (SINETABLE[inf_idx] * (1. - mu) + SINETABLE[sup_idx] * mu) * gain;
                phases[i] = phase + incr_phase;
            }

            buffer.push(sample);
        }
    }

    fn norm_factor(buffer: &[f32]) -> f32 {
        1. / buffer
            .iter()
            .copied()
            .reduce(f32::max)
            .expect("Maximum value not found")
    }

    /// computes the polyBLEP corrections to apply to aliasing signal
    ///
    /// polyBLEP stands for polyBandLimitedstEP
    fn poly_blep(&self, mut t: f32) -> f32 {
        let dt = self.incr_phase;
        if t < dt {
            t /= dt;
            t + t - t * t - 1.0
        } else if t > 1.0 - dt {
            t = (t - 1.0) / dt;
            t * t + t + t + 1.0
        } else {
            0.0
        }
    }
}

#[cfg(test)]

mod tests {
    use super::{PeriodicWave, PeriodicWaveOptions};
    use crate::context::AudioContext;

    #[test]
    #[should_panic]
    fn fails_to_build_when_real_is_too_short() {
        let context = AudioContext::new();

        let options = PeriodicWaveOptions {
            real: vec![0.],
            imag: vec![0., 0., 0.],
            disable_normalization: false,
        };

        let _periodic_wave = PeriodicWave::new(&context, Some(options));
    }

    #[test]
    #[should_panic]
    fn fails_to_build_when_imag_is_too_short() {
        let context = AudioContext::new();

        let options = PeriodicWaveOptions {
            real: vec![0., 0., 0.],
            imag: vec![0.],
            disable_normalization: false,
        };

        let _periodic_wave = PeriodicWave::new(&context, Some(options));
    }

    #[test]
    #[should_panic]
    fn fails_to_build_when_imag_and_real_not_equal_length() {
        let context = AudioContext::new();

        let options = PeriodicWaveOptions {
            real: vec![0., 0., 0.],
            imag: vec![0., 0.],
            disable_normalization: false,
        };

        let _periodic_wave = PeriodicWave::new(&context, Some(options));
    }
}
