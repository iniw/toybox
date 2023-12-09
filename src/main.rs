#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use embassy_stm32::{exti::*, gpio::*};
use static_cell::make_static;
use {defmt_rtt as _, panic_probe as _};

mod buttons;
mod leds;
mod recipes;
mod stations;

struct SharedContext {
    station_status_signals: stations::PerStationStaticData<stations::Signal>,
    led_signals: stations::PerStationStaticData<leds::Signal>,
    recipe_controller_channels: stations::PerStationStaticData<recipes::controller::Channel>,
    recipe_executor_channel: recipes::executor::Channel,
}

impl SharedContext {
    fn new() -> Self {
        Self {
            station_status_signals: stations::make_per_station_static_data!(stations::Signal::new()),
            led_signals: stations::make_per_station_static_data!(leds::Signal::new()),
            recipe_controller_channels: stations::make_per_station_static_data!(
                recipes::controller::Channel::new()
            ),
            recipe_executor_channel: recipes::executor::Channel::new(),
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let p = embassy_stm32::init(Default::default());

    let buttons_peripherals = [buttons::Peripherals {
        button_pin: p.PA0.degrade(),
        exti_channel: p.EXTI0.degrade(),
    }];

    let leds_peripherals = [leds::Peripherals {
        led_pin: p.PC6.degrade(),
    }];

    let ctx: &SharedContext = make_static!(SharedContext::new());

    recipes::spawn_tasks(&spawner, ctx);
    stations::spawn_tasks(&spawner, ctx);
    leds::spawn_tasks(&spawner, ctx, leds_peripherals);
    buttons::spawn_tasks(&spawner, ctx, buttons_peripherals);
}
