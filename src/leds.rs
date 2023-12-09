use defmt::{unreachable, *};
use embassy_executor::*;
use embassy_futures::select::{select, Either::*};
use embassy_stm32::gpio::*;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal};
use embassy_time::*;

use crate::stations;

struct Context {
    station: stations::Station,
    peripherals: Peripherals,
    led_signal: &'static Signal,
}

pub(super) struct Peripherals {
    pub led_pin: AnyPin,
}

#[derive(Format, Clone, Copy)]
pub enum Status {
    On,
    Off,
    Blinking,
}

impl Into<Level> for Status {
    fn into(self) -> Level {
        match self {
            Status::On => Level::High,
            Status::Off => Level::Low,
            Status::Blinking => unreachable!("not in a binary state"),
        }
    }
}

pub type Signal = signal::Signal<NoopRawMutex, Status>;

#[task(pool_size = stations::MAX_STATIONS)]
async fn main_task(ctx: Context) {
    let mut led = Output::new(ctx.peripherals.led_pin, Level::Low, Speed::Low);
    let mut led_status = Status::Off;
    loop {
        match led_status {
            Status::Blinking => {
                match select(Timer::after_millis(500), ctx.led_signal.wait()).await {
                    First(..) => {
                        led.toggle();
                        trace!("#{}: led blink", ctx.station);
                    }
                    Second(signal) => {
                        led_status = signal;
                    }
                }
            }
            Status::On | Status::Off => {
                led.set_level(led_status.into());
                trace!("#{}: led status = {}", ctx.station, led_status);
                led_status = ctx.led_signal.wait().await;
            }
        }
    }
}

pub(super) fn spawn_tasks(
    spawner: &Spawner,
    ctx: &'static crate::SharedContext,
    peripherals: stations::PerStationData<Peripherals>,
) {
    let mut iter = peripherals.into_iter();
    for i in 0..stations::MAX_STATIONS {
        unwrap!(spawner.spawn(main_task(Context {
            station: stations::Station(i),
            peripherals: unwrap!(iter.next()),
            led_signal: ctx.led_signals[i]
        })));
    }
}
