use glam::{Mat3, Quat, Vec3};

/// One semantic direction, as Rerun's `ViewCoordinates` uses them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisDirection {
    Up,
    Down,
    Right,
    Left,
    Forward,
    Back,
}

impl AxisDirection {
    /// The direction as a vector in OpenXR room space (X right, Y up, Z back).
    pub fn in_room(self) -> Vec3 {
        match self {
            Self::Up => Vec3::Y,
            Self::Down => Vec3::NEG_Y,
            Self::Right => Vec3::X,
            Self::Left => Vec3::NEG_X,
            Self::Forward => Vec3::NEG_Z,
            Self::Back => Vec3::Z,
        }
    }

    fn from_letter(letter: char) -> Option<Self> {
        match letter.to_ascii_uppercase() {
            'U' => Some(Self::Up),
            'D' => Some(Self::Down),
            'R' => Some(Self::Right),
            'L' => Some(Self::Left),
            'F' => Some(Self::Forward),
            'B' => Some(Self::Back),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordinatesError {
    Unparsable(String),
    RepeatedAxis,
    LeftHanded,
}

impl std::fmt::Display for CoordinatesError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unparsable(text) => write!(formatter, "not a view-coordinates code: {text:?}"),
            Self::RepeatedAxis => write!(formatter, "view coordinates repeat an axis"),
            Self::LeftHanded => write!(
                formatter,
                "left-handed view coordinates cannot be placed in the right-handed room without a mirror"
            ),
        }
    }
}

impl std::error::Error for CoordinatesError {}

/// What the X, Y and Z axes of Rerun's world mean, like Rerun's `ViewCoordinates`
/// (`RFU` is X right, Y forward, Z up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewCoordinates(pub [AxisDirection; 3]);

impl ViewCoordinates {
    /// Rerun's Z-up convention, common in robotics recordings.
    pub const RIGHT_FORWARD_UP: Self = Self([
        AxisDirection::Right,
        AxisDirection::Forward,
        AxisDirection::Up,
    ]);

    /// The OpenXR room convention itself.
    pub const RIGHT_UP_BACK: Self =
        Self([AxisDirection::Right, AxisDirection::Up, AxisDirection::Back]);

    pub fn parse(code: &str) -> Result<Self, CoordinatesError> {
        let letters: Vec<char> = code.trim().chars().collect();
        let [x_letter, y_letter, z_letter] = letters[..] else {
            return Err(CoordinatesError::Unparsable(code.to_owned()));
        };
        let parse_letter = |letter| {
            AxisDirection::from_letter(letter)
                .ok_or_else(|| CoordinatesError::Unparsable(code.to_owned()))
        };
        Ok(Self([
            parse_letter(x_letter)?,
            parse_letter(y_letter)?,
            parse_letter(z_letter)?,
        ]))
    }

    /// Rotation taking a vector in these coordinates to OpenXR room axes.
    pub fn room_from_coordinates(self) -> Result<Quat, CoordinatesError> {
        let basis = Mat3::from_cols(
            self.0[0].in_room(),
            self.0[1].in_room(),
            self.0[2].in_room(),
        );
        let determinant = basis.determinant();
        if determinant.abs() < 0.5 {
            return Err(CoordinatesError::RepeatedAxis);
        }
        if determinant < 0.0 {
            return Err(CoordinatesError::LeftHanded);
        }
        Ok(Quat::from_mat3(&basis))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-6;

    #[test]
    fn z_up_maps_to_room_y_up() {
        let rotation = ViewCoordinates::RIGHT_FORWARD_UP
            .room_from_coordinates()
            .unwrap();
        assert!((rotation * Vec3::Z).abs_diff_eq(Vec3::Y, EPSILON));
        assert!((rotation * Vec3::Y).abs_diff_eq(Vec3::NEG_Z, EPSILON));
        assert!((rotation * Vec3::X).abs_diff_eq(Vec3::X, EPSILON));
    }

    #[test]
    fn room_convention_is_identity() {
        let rotation = ViewCoordinates::RIGHT_UP_BACK
            .room_from_coordinates()
            .unwrap();
        assert!(rotation.abs_diff_eq(Quat::IDENTITY, EPSILON));
    }

    #[test]
    fn parses_rerun_codes() {
        assert_eq!(
            ViewCoordinates::parse("rfu").unwrap(),
            ViewCoordinates::RIGHT_FORWARD_UP
        );
        assert!(matches!(
            ViewCoordinates::parse("RF"),
            Err(CoordinatesError::Unparsable(_))
        ));
        assert!(matches!(
            ViewCoordinates::parse("RXU"),
            Err(CoordinatesError::Unparsable(_))
        ));
    }

    #[test]
    fn rejects_left_handed_and_degenerate_codes() {
        assert_eq!(
            ViewCoordinates::parse("RFD")
                .unwrap()
                .room_from_coordinates(),
            Err(CoordinatesError::LeftHanded)
        );
        assert_eq!(
            ViewCoordinates::parse("RRU")
                .unwrap()
                .room_from_coordinates(),
            Err(CoordinatesError::RepeatedAxis)
        );
    }

    #[test]
    fn every_right_handed_code_round_trips() {
        let letters = ['U', 'D', 'R', 'L', 'F', 'B'];
        let mut right_handed_count = 0;
        for x in letters {
            for y in letters {
                for z in letters {
                    let code: String = [x, y, z].iter().collect();
                    let coordinates = ViewCoordinates::parse(&code).unwrap();
                    let Ok(rotation) = coordinates.room_from_coordinates() else {
                        continue;
                    };
                    right_handed_count += 1;
                    for (axis_index, unit) in [Vec3::X, Vec3::Y, Vec3::Z].into_iter().enumerate() {
                        let expected = coordinates.0[axis_index].in_room();
                        assert!((rotation * unit).abs_diff_eq(expected, EPSILON), "{code}");
                        assert!(
                            (rotation.inverse() * expected).abs_diff_eq(unit, EPSILON),
                            "{code}"
                        );
                    }
                }
            }
        }
        assert_eq!(right_handed_count, 24);
    }
}
