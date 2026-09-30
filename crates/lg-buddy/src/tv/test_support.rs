//! A platform-neutral TV client for exercising policy and orchestration tests.

use super::{
    AudioStatus, CurrentInput, CurrentVolume, OledBrightness, TvClient, TvError, TvErrorKind,
    TvOperation, VolumeLevel,
};
use crate::config::HdmiInput;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

#[derive(Clone)]
pub(crate) struct FakeTvClient(Rc<RefCell<State>>);

struct State {
    power_on: bool,
    screen_on: bool,
    input: String,
    backlight: u64,
    volume: i16,
    muted: bool,
    calls: Vec<Call>,
    failures: HashMap<String, VecDeque<(TvErrorKind, String)>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Call {
    pub command: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub screen_on: bool,
    pub backlight: u8,
}

#[allow(dead_code)]
impl FakeTvClient {
    pub fn new(_label: &str) -> Self {
        Self(Rc::new(RefCell::new(State {
            power_on: true,
            screen_on: true,
            input: "HDMI_3".to_string(),
            backlight: 50,
            volume: 20,
            muted: false,
            calls: Vec::new(),
            failures: HashMap::new(),
        })))
    }

    pub fn set_power_on(&self, value: bool) {
        self.0.borrow_mut().power_on = value;
    }

    pub fn set_screen_on(&self, value: bool) {
        self.0.borrow_mut().screen_on = value;
    }

    pub fn set_input(&self, value: &str) {
        self.0.borrow_mut().input = value.to_string();
    }

    pub fn set_backlight(&self, value: u64) {
        self.0.borrow_mut().backlight = value;
    }

    pub fn set_volume(&self, value: i16) {
        self.0.borrow_mut().volume = value;
    }

    pub fn set_muted(&self, value: bool) {
        self.0.borrow_mut().muted = value;
    }

    pub fn queue_error(&self, command: &str, _status: i64, detail: &str) {
        self.queue_error_kind(command, TvErrorKind::Transport, detail);
    }

    pub fn queue_error_kind(&self, command: &str, kind: TvErrorKind, detail: &str) {
        self.0
            .borrow_mut()
            .failures
            .entry(command.to_string())
            .or_default()
            .push_back((kind, detail.to_string()));
    }

    pub fn calls(&self) -> Vec<Call> {
        self.0.borrow().calls.clone()
    }

    pub fn state_snapshot(&self) -> Snapshot {
        let state = self.0.borrow();
        Snapshot {
            screen_on: state.screen_on,
            backlight: state.backlight as u8,
        }
    }

    fn call(&self, name: &str, operation: TvOperation) -> Result<(), TvError> {
        let mut state = self.0.borrow_mut();
        state.calls.push(Call {
            command: name.to_string(),
        });
        if let Some((kind, detail)) = state.failures.get_mut(name).and_then(VecDeque::pop_front) {
            return Err(TvError::new(operation, kind, detail));
        }
        Ok(())
    }
}

impl TvClient for FakeTvClient {
    fn model_name(&self) -> Result<String, TvError> {
        self.call("model_name", TvOperation::ReadModelName)?;
        Ok("TEST TV".to_string())
    }

    fn current_input(&self) -> Result<CurrentInput, TvError> {
        self.call("current_input", TvOperation::ReadInput)?;
        let state = self.0.borrow();
        if !state.power_on {
            return Err(TvError::new(
                TvOperation::ReadInput,
                TvErrorKind::Transport,
                "TV powered off",
            ));
        }
        Ok(match state.input.parse::<HdmiInput>() {
            Ok(input) => CurrentInput::Hdmi(input),
            Err(_) => CurrentInput::Other(state.input.clone()),
        })
    }

    fn oled_brightness(&self) -> Result<OledBrightness, TvError> {
        self.call("oled_brightness", TvOperation::ReadOledBrightness)?;
        OledBrightness::new(self.0.borrow().backlight as u8).map_err(|error| {
            TvError::new(
                TvOperation::ReadOledBrightness,
                TvErrorKind::InvalidResponse,
                error.to_string(),
            )
        })
    }

    fn audio_status(&self) -> Result<AudioStatus, TvError> {
        self.call("audio_status", TvOperation::ReadAudioStatus)?;
        let state = self.0.borrow();
        let volume = if state.volume < 0 {
            CurrentVolume::Unknown
        } else {
            CurrentVolume::Level(VolumeLevel::new(state.volume as u8).map_err(|error| {
                TvError::new(
                    TvOperation::ReadAudioStatus,
                    TvErrorKind::InvalidResponse,
                    error.to_string(),
                )
            })?)
        };
        Ok(AudioStatus::new(volume, state.muted))
    }

    fn set_input(&self, input: HdmiInput) -> Result<(), TvError> {
        self.call("set_input", TvOperation::SetInput)?;
        let mut state = self.0.borrow_mut();
        if !state.screen_on {
            return Err(TvError::new(
                TvOperation::SetInput,
                TvErrorKind::ScreenNotVisible,
                "screen is off",
            ));
        }
        state.power_on = true;
        state.input = input.as_str().to_string();
        Ok(())
    }

    fn set_oled_brightness(&self, brightness: OledBrightness) -> Result<(), TvError> {
        self.call("set_oled_brightness", TvOperation::SetOledBrightness)?;
        self.0.borrow_mut().backlight = u64::from(brightness.as_percent());
        Ok(())
    }

    fn set_volume(&self, volume: VolumeLevel) -> Result<(), TvError> {
        self.call("set_volume", TvOperation::SetVolume)?;
        self.0.borrow_mut().volume = i16::from(volume.as_percent());
        Ok(())
    }

    fn volume_up(&self) -> Result<(), TvError> {
        self.call("volume_up", TvOperation::VolumeUp)?;
        self.0.borrow_mut().volume += 1;
        Ok(())
    }

    fn volume_down(&self) -> Result<(), TvError> {
        self.call("volume_down", TvOperation::VolumeDown)?;
        self.0.borrow_mut().volume -= 1;
        Ok(())
    }

    fn set_muted(&self, muted: bool) -> Result<(), TvError> {
        self.call("set_muted", TvOperation::SetMuted)?;
        self.0.borrow_mut().muted = muted;
        Ok(())
    }

    fn power_off(&self) -> Result<(), TvError> {
        self.call("power_off", TvOperation::PowerOff)?;
        let mut state = self.0.borrow_mut();
        state.power_on = false;
        state.screen_on = false;
        Ok(())
    }

    fn blank_screen(&self) -> Result<(), TvError> {
        self.call("blank_screen", TvOperation::BlankScreen)?;
        self.0.borrow_mut().screen_on = false;
        Ok(())
    }

    fn unblank_screen(&self) -> Result<(), TvError> {
        self.call("unblank_screen", TvOperation::UnblankScreen)?;
        let mut state = self.0.borrow_mut();
        if !state.power_on {
            return Err(TvError::new(
                TvOperation::UnblankScreen,
                TvErrorKind::Transport,
                "TV powered off",
            ));
        }
        state.screen_on = true;
        Ok(())
    }
}
