use defmt::{debug, trace, unreachable, unwrap, Format};
use embassy_executor::{task, Spawner};
use embassy_futures::select::{select, Either::*};
use embassy_stm32::gpio::{AnyPin, Level, Output, Pin, Speed};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::Timer;

use crate::stations::{PerStationData, PerStationStaticData, Station, MAX_NUM_STATIONS};

#[task(pool_size = MAX_NUM_STATIONS)]
async fn led_task(ctx: LedControlContext) {
    let mut led = Output::new(ctx.peripherals.led_pin, Level::Low, Speed::Low);
    let mut led_status = LedStatus::Off;

    loop {
        match led_status {
            LedStatus::Blinking => {
                match select(Timer::after_millis(500), ctx.led_signal.wait()).await {
                    First(_) => {
                        led.toggle();
                        trace!("led #{} blink", ctx.station);
                    }
                    Second(signal) => {
                        led_status = signal;
                    }
                }
            }
            LedStatus::On | LedStatus::Off => {
                led.set_level(led_status.into());
                trace!("led #{} status = {}", ctx.station, led_status);
                led_status = ctx.led_signal.wait().await;
            }
        }
    }
}

#[derive(Format, Clone, Copy)]
pub enum LedStatus {
    On,
    Off,
    Blinking,
}

impl Into<Level> for LedStatus {
    fn into(self) -> Level {
        match self {
            LedStatus::On => Level::High,
            LedStatus::Off => Level::Low,
            LedStatus::Blinking => unreachable!("not in a binary state"),
        }
    }
}

pub type LedSignal = Signal<NoopRawMutex, LedStatus>;

pub(super) struct Peripherals {
    pub(super) led_pin: AnyPin,
}

struct LedControlContext {
    station: Station,
    peripherals: Peripherals,
    led_signal: &'static LedSignal,
}

pub fn spawn_tasks(
    spawner: &Spawner,
    peripherals: PerStationData<Peripherals>,
    led_signals: &'static PerStationStaticData<LedSignal>,
) {
    let mut iter = peripherals.into_iter();
    for i in 0..MAX_NUM_STATIONS {
        unwrap!(spawner.spawn(led_task(LedControlContext {
            station: Station(i),
            peripherals: unwrap!(iter.next()),
            led_signal: led_signals[i]
        })));
    }
}
