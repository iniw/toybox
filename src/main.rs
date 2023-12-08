#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(let_chains)]

use defmt_rtt as _;
use embassy_stm32::exti::*;
use embassy_stm32::gpio::Pin;
use panic_probe as _;

use crate::leds::LedSignal;
use crate::recipes::RecipeControllerChannel;
use crate::stations::StationStatusSignal;

mod buttons;
mod leds;
mod recipes;
mod stations;

struct GlobalContext {
    station_status_signals: stations::PerStationStaticData<StationStatusSignal>,
    recipe_controller_channels: stations::PerStationStaticData<RecipeControllerChannel>,
    led_signals: stations::PerStationStaticData<LedSignal>,
}

impl GlobalContext {
    fn new() -> Self {
        Self {
            station_status_signals: stations::make_per_station_static_data!(
                StationStatusSignal::new()
            ),
            recipe_controller_channels: stations::make_per_station_static_data!(
                RecipeControllerChannel::new()
            ),
            led_signals: stations::make_per_station_static_data!(LedSignal::new()),
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let p = embassy_stm32::init(Default::default());

    let ctx: &'static GlobalContext = static_cell::make_static!(GlobalContext::new());
    let buttons_peripherals = [buttons::Peripherals {
        button_pin: p.PA0.degrade(),
        exti_channel: p.EXTI0.degrade(),
    }];
    let leds_peripherals = [leds::Peripherals {
        led_pin: p.PC6.degrade(),
    }];

    recipes::spawn_tasks(&spawner, &ctx);
    stations::spawn_tasks(&spawner, &ctx);
    leds::spawn_tasks(&spawner, leds_peripherals, &ctx);
    buttons::spawn_tasks(&spawner, buttons_peripherals, &ctx);
}
