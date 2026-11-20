use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Px(pub f64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub [f64; 4]);

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Seconds(f64);

impl Seconds {
    pub const ZERO: Self = Self(0.0);

    pub fn new(value: f64) -> Result<Self, AnimationError> {
        if value.is_finite() && value >= 0.0 {
            Ok(Self(value))
        } else {
            Err(AnimationError::InvalidTime)
        }
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Phase(f64);

impl Phase {
    pub fn new(value: f64) -> Result<Self, AnimationError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(AnimationError::InvalidPhase)
        }
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationError {
    InvalidTime,
    InvalidPhase,
    InvalidSpring,
    NonFiniteValue,
}

impl fmt::Display for AnimationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidTime => "time must be finite and nonnegative",
            Self::InvalidPhase => "phase must be finite and between zero and one",
            Self::InvalidSpring => {
                "spring frequency, damping and tolerance must be finite and positive"
            }
            Self::NonFiniteValue => "animated values must be finite",
        })
    }
}

impl Error for AnimationError {}

/// A finite vector space whose components share one animation clock.
pub trait AnimationValue: Copy + PartialEq {
    fn add_scaled(self, other: Self, factor: f64) -> Self;
    fn scaled(self, factor: f64) -> Self;
    fn magnitude(self) -> f64;
}

impl AnimationValue for f64 {
    fn add_scaled(self, other: Self, factor: f64) -> Self {
        self + other * factor
    }

    fn scaled(self, factor: f64) -> Self {
        self * factor
    }

    fn magnitude(self) -> f64 {
        self.abs()
    }
}

impl AnimationValue for Px {
    fn add_scaled(self, other: Self, factor: f64) -> Self {
        Self(self.0 + other.0 * factor)
    }

    fn scaled(self, factor: f64) -> Self {
        Self(self.0 * factor)
    }

    fn magnitude(self) -> f64 {
        self.0.abs()
    }
}

impl AnimationValue for Rgba {
    fn add_scaled(self, other: Self, factor: f64) -> Self {
        Self(std::array::from_fn(|index| {
            self.0[index] + other.0[index] * factor
        }))
    }

    fn scaled(self, factor: f64) -> Self {
        Self(self.0.map(|value| value * factor))
    }

    fn magnitude(self) -> f64 {
        self.0.iter().map(|value| value.abs()).sum()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Easing {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Easing {
    pub fn sample(self, phase: Phase) -> f64 {
        let time = phase.get();
        match self {
            Self::Linear => time,
            Self::EaseIn => time * time,
            Self::EaseOut => time * (2.0 - time),
            Self::EaseInOut => time * time * (3.0 - 2.0 * time),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    angular_frequency: f64,
    damping_ratio: f64,
    tolerance: f64,
}

impl Spring {
    pub fn new(
        frequency_hz: f64,
        damping_ratio: f64,
        tolerance: f64,
    ) -> Result<Self, AnimationError> {
        let angular_frequency = std::f64::consts::TAU * frequency_hz;
        if [angular_frequency, damping_ratio, tolerance]
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
            && (angular_frequency * angular_frequency).is_finite()
            && (damping_ratio * damping_ratio).is_finite()
            && (angular_frequency * damping_ratio).is_finite()
            && (angular_frequency * (2.0 * damping_ratio)).is_finite()
        {
            Ok(Self {
                angular_frequency,
                damping_ratio,
                tolerance,
            })
        } else {
            Err(AnimationError::InvalidSpring)
        }
    }

    fn step<T: AnimationValue>(self, displacement: T, velocity: T, time: f64) -> (T, T) {
        let omega = self.angular_frequency;
        let damping = self.damping_ratio;
        let decay_rate = omega * damping;
        let decay = (-decay_rate * time).exp();
        if decay == 0.0 && damping <= 1.0 {
            return (displacement.scaled(0.0), velocity.scaled(0.0));
        }
        if damping == 1.0 {
            let tangent = velocity.add_scaled(displacement, omega);
            (
                displacement.add_scaled(tangent, time).scaled(decay),
                velocity.add_scaled(tangent, -omega * time).scaled(decay),
            )
        } else if damping < 1.0 {
            let damped_frequency = omega * (1.0 - damping * damping).sqrt();
            let angle = (time % (std::f64::consts::TAU / damped_frequency)) * damped_frequency;
            let sine = angle.sin() / damped_frequency;
            let cosine = angle.cos();
            (
                displacement
                    .scaled(cosine + decay_rate * sine)
                    .add_scaled(velocity, sine)
                    .scaled(decay),
                velocity
                    .scaled(cosine - decay_rate * sine)
                    .add_scaled(displacement, -omega * omega * sine)
                    .scaled(decay),
            )
        } else {
            let root = (damping * damping - 1.0).sqrt();
            // This form avoids cancellation in the slow root for heavy damping.
            let slow = -omega / (damping + root);
            let fast = -omega * (damping + root);
            let slow_decay = (slow * time).exp();
            // exp_m1 preserves the difference of nearly equal roots near critical damping.
            let sine = slow_decay * -((fast - slow) * time).exp_m1() / (slow - fast);
            (
                displacement
                    .scaled(slow_decay - slow * sine)
                    .add_scaled(velocity, sine),
                velocity
                    .scaled(slow_decay + fast * sine)
                    .add_scaled(displacement, -omega * omega * sine),
            )
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transition {
    Immediate,
    Tween { duration: Seconds, easing: Easing },
    Spring(Spring),
}

#[derive(Clone, Copy, Debug)]
enum Motion<T> {
    Idle,
    Tween {
        start: T,
        elapsed: f64,
        duration: Seconds,
        easing: Easing,
    },
    Spring {
        settings: Spring,
        velocity: T,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameChange {
    pub changed: bool,
    pub active: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Prop<T> {
    value: T,
    target: T,
    motion: Motion<T>,
}

impl<T: AnimationValue> Prop<T> {
    pub fn new(value: T) -> Result<Self, AnimationError> {
        if !value.magnitude().is_finite() {
            return Err(AnimationError::NonFiniteValue);
        }
        Ok(Self {
            value,
            target: value,
            motion: Motion::Idle,
        })
    }

    pub fn value(&self) -> T {
        self.value
    }

    pub fn target(&self) -> T {
        self.target
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.motion, Motion::Idle)
    }

    pub fn transition_to(
        &mut self,
        target: T,
        transition: Transition,
    ) -> Result<FrameChange, AnimationError> {
        if !target.magnitude().is_finite()
            || !self.value.add_scaled(target, -1.0).magnitude().is_finite()
        {
            return Err(AnimationError::NonFiniteValue);
        }
        let before = self.value;
        self.target = target;
        let velocity = match self.motion {
            Motion::Spring { velocity, .. } => velocity,
            _ => self.value.scaled(0.0),
        };
        self.motion = match transition {
            Transition::Immediate => {
                self.value = target;
                Motion::Idle
            }
            Transition::Tween { duration, .. } if duration == Seconds::ZERO => {
                self.value = target;
                Motion::Idle
            }
            Transition::Tween { duration, easing } if self.value != target => Motion::Tween {
                start: self.value,
                elapsed: 0.0,
                duration,
                easing,
            },
            Transition::Spring(settings)
                if self.value != target || velocity.magnitude() > settings.tolerance =>
            {
                Motion::Spring { settings, velocity }
            }
            _ => Motion::Idle,
        };
        Ok(FrameChange {
            changed: self.value != before,
            active: self.is_active(),
        })
    }

    pub fn advance(&mut self, delta: Seconds) -> FrameChange {
        let before = self.value;
        if delta == Seconds::ZERO {
            return FrameChange {
                changed: false,
                active: self.is_active(),
            };
        }
        match &mut self.motion {
            Motion::Idle => {}
            Motion::Tween {
                start,
                elapsed,
                duration,
                easing,
            } => {
                *elapsed = (*elapsed + delta.get()).min(duration.get());
                let phase = Phase(*elapsed / duration.get());
                let fraction = easing.sample(phase);
                self.value = start
                    .scaled(1.0 - fraction)
                    .add_scaled(self.target, fraction);
                if *elapsed >= duration.get() {
                    self.value = self.target;
                    self.motion = Motion::Idle;
                }
            }
            Motion::Spring { settings, velocity } => {
                let displacement = self.value.add_scaled(self.target, -1.0);
                let (next_displacement, next_velocity) =
                    settings.step(displacement, *velocity, delta.get());
                self.value = self.target.add_scaled(next_displacement, 1.0);
                *velocity = next_velocity;
                if next_displacement.magnitude() <= settings.tolerance
                    && next_velocity.magnitude() <= settings.tolerance
                {
                    self.value = self.target;
                    self.motion = Motion::Idle;
                }
            }
        }
        FrameChange {
            changed: self.value != before,
            active: self.is_active(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(value: f64) -> Seconds {
        Seconds::new(value).expect("valid test time")
    }

    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1.0e-9, "{actual} != {expected}");
    }

    #[test]
    fn easing_endpoints_and_monotonicity() {
        for easing in [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
        ] {
            close(
                easing.sample(Phase::new(0.0).expect("valid test value")),
                0.0,
            );
            close(
                easing.sample(Phase::new(1.0).expect("valid test value")),
                1.0,
            );
            let mut previous = 0.0;
            for step in 0..=100 {
                let current =
                    easing.sample(Phase::new(f64::from(step) / 100.0).expect("valid test value"));
                assert!(current >= previous);
                previous = current;
            }
        }
    }

    #[test]
    fn tween_finishes_exactly_and_stops_damage() {
        let mut prop = Prop::new(Px(10.0)).expect("valid test value");
        prop.transition_to(
            Px(30.0),
            Transition::Tween {
                duration: seconds(1.0),
                easing: Easing::Linear,
            },
        )
        .expect("valid test value");
        assert_eq!(
            prop.advance(seconds(0.25)),
            FrameChange {
                changed: true,
                active: true
            }
        );
        close(prop.value().0, 15.0);
        assert_eq!(
            prop.advance(seconds(10.0)),
            FrameChange {
                changed: true,
                active: false
            }
        );
        assert_eq!(prop.value(), Px(30.0));
        assert_eq!(
            prop.advance(seconds(1.0)),
            FrameChange {
                changed: false,
                active: false
            }
        );
    }

    #[test]
    fn tween_retargets_from_the_visible_value() {
        let mut prop = Prop::new(Px(0.0)).expect("valid test value");
        let transition = Transition::Tween {
            duration: seconds(1.0),
            easing: Easing::Linear,
        };
        prop.transition_to(Px(100.0), transition)
            .expect("valid test value");
        prop.advance(seconds(0.4));
        let before = prop.value();
        assert!(
            !prop
                .transition_to(Px(-60.0), transition)
                .expect("valid test value")
                .changed
        );
        assert_eq!(prop.value(), before);
        prop.advance(seconds(0.5));
        close(prop.value().0, -10.0);
    }

    #[test]
    fn spring_retarget_preserves_velocity() {
        let settings = Spring::new(2.0, 0.7, 0.0001).expect("valid test value");
        let mut prop = Prop::new(Px(0.0)).expect("valid test value");
        prop.transition_to(Px(100.0), Transition::Spring(settings))
            .expect("valid test value");
        prop.advance(seconds(0.1));
        let before = prop.value();
        let velocity = match prop.motion {
            Motion::Spring { velocity, .. } => velocity,
            _ => panic!("spring active"),
        };
        prop.transition_to(Px(-100.0), Transition::Spring(settings))
            .expect("valid test value");
        assert_eq!(prop.value(), before);
        match prop.motion {
            Motion::Spring {
                velocity: after, ..
            } => assert_eq!(after, velocity),
            _ => panic!("retargeted spring active"),
        }
    }

    #[test]
    fn springs_converge_for_every_damping_regime() {
        for damping in [0.3, 1.0, 2.0] {
            let mut prop = Prop::new(Px(0.0)).expect("valid test value");
            prop.transition_to(
                Px(150.0),
                Transition::Spring(Spring::new(3.0, damping, 0.001).expect("valid test value")),
            )
            .expect("valid test value");
            for _ in 0..1200 {
                prop.advance(seconds(1.0 / 60.0));
            }
            assert_eq!(prop.value(), Px(150.0));
            assert!(!prop.is_active());
        }
    }

    #[test]
    fn spring_motion_is_independent_of_frame_partition() {
        for damping in [0.4, 1.0 - 1.0e-10, 1.0, 1.0 + 1.0e-10, 1.8] {
            let mut single = Prop::new(Px(-20.0)).expect("valid test value");
            single
                .transition_to(
                    Px(120.0),
                    Transition::Spring(
                        Spring::new(2.0, damping, 1.0e-12).expect("valid test value"),
                    ),
                )
                .expect("valid test value");
            let mut split = single;
            single.advance(seconds(0.5));
            for delta in [0.017, 0.083, 0.1, 0.05, 0.25] {
                split.advance(seconds(delta));
            }
            close(single.value().0, split.value().0);
        }
    }

    #[test]
    fn immediate_and_zero_duration_changes_need_one_redraw() {
        let mut prop = Prop::new(Px(1.0)).expect("valid test value");
        assert_eq!(
            prop.transition_to(Px(2.0), Transition::Immediate)
                .expect("valid test value"),
            FrameChange {
                changed: true,
                active: false
            }
        );
        assert_eq!(
            prop.transition_to(
                Px(3.0),
                Transition::Tween {
                    duration: Seconds::ZERO,
                    easing: Easing::Linear
                }
            )
            .expect("valid test value"),
            FrameChange {
                changed: true,
                active: false
            }
        );
        assert_eq!(
            prop.transition_to(Px(3.0), Transition::Immediate)
                .expect("valid test value"),
            FrameChange {
                changed: false,
                active: false
            }
        );
    }

    #[test]
    fn zero_time_preserves_active_motion() {
        let mut prop = Prop::new(Px(1.0)).expect("valid test value");
        prop.transition_to(
            Px(2.0),
            Transition::Spring(Spring::new(2.0, 1.0, 0.001).expect("valid test value")),
        )
        .expect("valid test value");
        assert_eq!(
            prop.advance(Seconds::ZERO),
            FrameChange {
                changed: false,
                active: true
            }
        );
        assert_eq!(prop.value(), Px(1.0));
    }

    #[test]
    fn colors_share_a_clock_and_preserve_exact_target() {
        let mut prop = Prop::new(Rgba([0.0, 1.0, 0.0, 1.0])).expect("valid test value");
        let target = Rgba([1.0, 0.0, 0.5, 0.0]);
        prop.transition_to(
            target,
            Transition::Tween {
                duration: seconds(1.0),
                easing: Easing::Linear,
            },
        )
        .expect("valid test value");
        prop.advance(seconds(0.5));
        assert_eq!(prop.value(), Rgba([0.5, 0.5, 0.25, 0.5]));
        prop.advance(seconds(0.5));
        assert_eq!(prop.value(), target);
    }

    #[test]
    fn a_long_pause_settles_springs_without_nonfinite_positions() {
        for damping in [0.3, 1.0, 2.0] {
            let mut prop = Prop::new(Px(0.0)).expect("valid test value");
            prop.transition_to(
                Px(100.0),
                Transition::Spring(Spring::new(2.0, damping, 0.001).expect("valid test value")),
            )
            .expect("valid test value");
            assert_eq!(
                prop.advance(seconds(1.0e100)),
                FrameChange {
                    changed: true,
                    active: false,
                }
            );
            assert_eq!(prop.value(), Px(100.0));
        }
    }

    #[test]
    fn invalid_inputs_are_rejected_without_corrupting_motion() {
        for value in [f64::NAN, f64::INFINITY, -1.0] {
            assert!(Seconds::new(value).is_err());
        }
        for value in [f64::NAN, -0.1, 1.1] {
            assert!(Phase::new(value).is_err());
        }
        for values in [
            (0.0, 1.0, 0.001),
            (2.0, -1.0, 0.001),
            (2.0, 1.0, 0.0),
            (f64::MAX, 1.0, 0.001),
        ] {
            assert!(Spring::new(values.0, values.1, values.2).is_err());
        }
        assert!(Prop::new(Rgba([0.0, 0.0, f64::NAN, 1.0])).is_err());
        let mut prop = Prop::new(Px(1.0)).expect("valid test value");
        assert!(
            prop.transition_to(Px(f64::INFINITY), Transition::Immediate)
                .is_err()
        );
        assert_eq!(prop.value(), Px(1.0));
        assert_eq!(prop.target(), Px(1.0));
    }
}
