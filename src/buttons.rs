use defmt::*;
use embassy_executor::*;
use embassy_futures::select::{select, Either::*};
use embassy_stm32::{exti, exti::ExtiInput, gpio::*};
use embassy_time::*;

use crate::recipes;
use crate::stations;

struct Context {
    station: stations::Station,
    peripherals: Peripherals,
    recipe_controller_signal: &'static recipes::controller::Channel,
}

pub(super) struct Peripherals {
    pub button_pin: AnyPin,
    pub exti_channel: exti::AnyChannel,
}

#[task(pool_size = stations::MAX_STATIONS)]
async fn main_task(ctx: Context) {
    let input = Input::new(ctx.peripherals.button_pin, Pull::Up);
    let mut button = ExtiInput::new(input, ctx.peripherals.exti_channel);

    loop {
        button.wait_for_rising_edge().await;
        trace!("#{}: button pressed", ctx.station);

        match select(button.wait_for_falling_edge(), Timer::after_secs(3)).await {
            First(..) => {
                trace!("#{}: button released", ctx.station);
                ctx.recipe_controller_signal
                    .send(recipes::controller::Event::ButtonPress)
                    .await;
            }
            Second(..) => {
                trace!("#{}: button was held", ctx.station);
                ctx.recipe_controller_signal
                    .send(recipes::controller::Event::CancelRecipe)
                    .await;
            }
        }

        // disallow quick button presses since the signal is edge-triggered
        // so we might run into false-positives on bad hardware
        Timer::after_millis(100).await;
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
            recipe_controller_signal: ctx.recipe_controller_channels[i]
        })));
    }
}
