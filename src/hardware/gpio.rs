use super::{
    CapturedAt, EdgeSink, HardwareConfig, HardwareError, PowerOutput, PullMode, RawEdge,
    TimingSource,
};
use crate::domain::SignalEdge;
use rppal::gpio::{Gpio, InputPin, Level, OutputPin, Trigger};

pub struct GpioTimingSource {
    config: HardwareConfig,
    pins: Vec<(u8, InputPin)>,
}

impl GpioTimingSource {
    pub fn new(config: HardwareConfig) -> Self {
        Self {
            config,
            pins: Vec::new(),
        }
    }
}

impl TimingSource for GpioTimingSource {
    fn start(&mut self, sink: EdgeSink) -> Result<(), HardwareError> {
        let gpio = Gpio::new().map_err(gpio_error)?;
        for mapping in &self.config.lanes {
            let pin = gpio.get(mapping.input.bcm_pin).map_err(gpio_error)?;
            let mut pin = match mapping.input.pull {
                PullMode::Off => pin.into_input(),
                PullMode::Up => pin.into_input_pullup(),
                PullMode::Down => pin.into_input_pulldown(),
            };
            let lane = mapping.lane;
            let sink = sink.clone();
            pin.set_async_interrupt(Trigger::Both, None, move |event| {
                let captured_at = event.timestamp;
                let edge = match event.trigger {
                    Trigger::RisingEdge => SignalEdge::Rising,
                    Trigger::FallingEdge => SignalEdge::Falling,
                    _ => return,
                };
                let _ = sink.accept(RawEdge {
                    lane,
                    edge,
                    captured_at: CapturedAt::KernelMonotonic(captured_at),
                });
            })
            .map_err(gpio_error)?;
            self.pins.push((lane, pin));
        }
        Ok(())
    }

    fn initial_levels(&self) -> Vec<(u8, bool)> {
        self.pins
            .iter()
            .map(|(lane, pin)| (*lane, pin.is_high()))
            .collect()
    }
}

pub struct GpioPowerOutput {
    pins: Vec<(u8, bool, OutputPin)>,
}

impl GpioPowerOutput {
    pub fn new(config: &HardwareConfig) -> Result<Self, HardwareError> {
        let gpio = Gpio::new().map_err(gpio_error)?;
        let mut pins = Vec::with_capacity(config.lanes.len());
        for mapping in &config.lanes {
            let pin = gpio.get(mapping.relay.bcm_pin).map_err(gpio_error)?;
            let pin = if mapping.relay.active_high {
                pin.into_output_low()
            } else {
                pin.into_output_high()
            };
            pins.push((mapping.lane, mapping.relay.active_high, pin));
        }
        Ok(Self { pins })
    }
}

impl PowerOutput for GpioPowerOutput {
    fn set_lane_power(&mut self, lanes: [bool; 4]) -> Result<(), HardwareError> {
        for (lane, active_high, pin) in &mut self.pins {
            let active = lanes[usize::from(*lane - 1)];
            pin.write(if active == *active_high {
                Level::High
            } else {
                Level::Low
            });
        }
        Ok(())
    }
}

fn gpio_error(error: rppal::gpio::Error) -> HardwareError {
    HardwareError::new(error.to_string())
}
