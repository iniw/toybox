use defmt::{trace, unwrap};
use embassy_executor::task;
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either::*};
use embassy_stm32::exti;
use embassy_stm32::gpio::{AnyPin, Input, Pull};
use embassy_time::Timer;

use crate::recipes::*;
use crate::stations::*;

#[task(pool_size = MAX_NUM_STATIONS)]
async fn button_controller_task(ctx: ButtonControllerContext) {
    let input = Input::new(ctx.peripherals.button_pin, Pull::Up);
    let mut button = exti::ExtiInput::new(input, ctx.peripherals.exti_channel);

    loop {
        button.wait_for_rising_edge().await;
        trace!("button #{} pressed", ctx.station);

        match select(button.wait_for_falling_edge(), Timer::after_secs(3)).await {
            First(_) => {
                trace!("button #{} released", ctx.station);
                ctx.recipe_controller_signal
                    .signal(RecipeControllerEvent::ButtonPress);
            }
            Second(_) => {
                trace!("button #{} was held", ctx.station);
                ctx.recipe_controller_signal
                    .signal(RecipeControllerEvent::CancelRecipe);
            }
        }

        // disallow quick button presses since the signal is edge-triggered
        // so we might run into false-positives on bad hardware
        Timer::after_millis(100).await;
    }
}

pub(super) struct Peripherals {
    pub(super) button_pin: AnyPin,
    pub(super) exti_channel: exti::AnyChannel,
}

struct ButtonControllerContext {
    station: Station,
    peripherals: Peripherals,
    recipe_controller_signal: &'static RecipeControllerSignal,
}

pub fn spawn_tasks(
    spawner: &Spawner,
    peripherals: PerStationData<Peripherals>,
    ctx: &'static crate::GlobalContext,
) {
    let mut iter = peripherals.into_iter();
    for i in 0..MAX_NUM_STATIONS {
        unwrap!(
            spawner.spawn(button_controller_task(ButtonControllerContext {
                station: Station(i),
                peripherals: unwrap!(iter.next()),
                recipe_controller_signal: ctx.recipe_controller_signals[i]
            }))
        );
    }
}
