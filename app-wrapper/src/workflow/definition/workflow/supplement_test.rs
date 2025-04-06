#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn test_duration_to_millis() {
        let duration = Duration::Inline {
            days: Some(1),
            hours: Some(2),
            minutes: Some(3),
            seconds: Some(4),
            milliseconds: Some(5),
        };
        assert_eq!(duration.to_millis(), 93784005);
    }

    #[test]
    fn test_duration_from_millis() {
        let duration = Duration::from_millis(93784005);
        if let Duration::Inline {
            days,
            hours,
            minutes,
            seconds,
            milliseconds,
        } = duration
        {
            assert_eq!(days, Some(1));
            assert_eq!(hours, Some(2));
            assert_eq!(minutes, Some(3));
            assert_eq!(seconds, Some(4));
            assert_eq!(milliseconds, Some(5));
        } else {
            panic!("Expected Duration::Inline");
        }
    }

    #[test]
    fn test_duration_roundtrip() {
        let original_ms = 123456789;
        let duration = Duration::from_millis(original_ms);
        let converted_ms = duration.to_millis();
        assert_eq!(original_ms, converted_ms);
    }

    #[test]
    fn test_duration_expression() {
        let duration = Duration::Expression(DurationExpression("P1DT2H3M4.5S".to_string()));
        let ms = duration.to_millis();
        assert_eq!(ms, 93784500);
    }

    #[test]
    fn test_zero_duration() {
        let duration = Duration::from_millis(0);
        if let Duration::Inline {
            days,
            hours,
            minutes,
            seconds,
            milliseconds,
        } = duration
        {
            assert_eq!(days, Some(0));
            assert_eq!(hours, Some(0));
            assert_eq!(minutes, Some(0));
            assert_eq!(seconds, Some(0));
            assert_eq!(milliseconds, Some(0));
        } else {
            panic!("Expected Duration::Inline");
        }
        assert_eq!(duration.to_millis(), 0);
    }

    #[test]
    fn test_large_duration() {
        // Test a large duration value
        let large_ms = u64::MAX / 2; // Using a large but safe value
        let duration = Duration::from_millis(large_ms);
        assert_eq!(duration.to_millis(), large_ms);
    }
    #[test]
    fn test_duration_inline_to_millis() {
        // Test with days only
        let duration = Duration::Inline {
            days: Some(1),
            hours: None,
            minutes: None,
            seconds: None,
            milliseconds: None,
        };
        assert_eq!(duration.to_millis(), 24 * 60 * 60 * 1000); // 1 day = 86,400,000 ms

        // Test with hours only
        let duration = Duration::Inline {
            days: None,
            hours: Some(2),
            minutes: None,
            seconds: None,
            milliseconds: None,
        };
        assert_eq!(duration.to_millis(), 2 * 60 * 60 * 1000); // 2 hours = 7,200,000 ms

        // Test with minutes only
        let duration = Duration::Inline {
            days: None,
            hours: None,
            minutes: Some(30),
            seconds: None,
            milliseconds: None,
        };
        assert_eq!(duration.to_millis(), 30 * 60 * 1000); // 30 minutes = 1,800,000 ms

        // Test with seconds only
        let duration = Duration::Inline {
            days: None,
            hours: None,
            minutes: None,
            seconds: Some(45),
            milliseconds: None,
        };
        assert_eq!(duration.to_millis(), 45 * 1000); // 45 seconds = 45,000 ms

        // Test with milliseconds only
        let duration = Duration::Inline {
            days: None,
            hours: None,
            minutes: None,
            seconds: None,
            milliseconds: Some(500),
        };
        assert_eq!(duration.to_millis(), 500); // 500 ms

        // Test with combination of fields
        let duration = Duration::Inline {
            days: Some(1),
            hours: Some(12),
            minutes: Some(30),
            seconds: Some(45),
            milliseconds: Some(500),
        };
        let expected = 24 * 60 * 60 * 1000 + // 1 day
            12 * 60 * 60 * 1000 +     // 12 hours
            30 * 60 * 1000 +          // 30 minutes
            45 * 1000 +               // 45 seconds
            500; // 500 ms
        assert_eq!(duration.to_millis(), expected);
    }

    #[test]
    fn test_duration_expression_to_millis() {
        // Test simple expressions
        let duration = Duration::Expression(DurationExpression("P1D".to_string()));
        assert_eq!(duration.to_millis(), 24 * 60 * 60 * 1000); // 1 day = 86,400,000 ms

        let duration = Duration::Expression(DurationExpression("PT2H".to_string()));
        assert_eq!(duration.to_millis(), 2 * 60 * 60 * 1000); // 2 hours = 7,200,000 ms

        let duration = Duration::Expression(DurationExpression("PT30M".to_string()));
        assert_eq!(duration.to_millis(), 30 * 60 * 1000); // 30 minutes = 1,800,000 ms

        let duration = Duration::Expression(DurationExpression("PT45S".to_string()));
        assert_eq!(duration.to_millis(), 45 * 1000); // 45 seconds = 45,000 ms

        // Test combined expressions
        let duration = Duration::Expression(DurationExpression("P1DT12H30M45S".to_string()));
        let expected = 24 * 60 * 60 * 1000 + // 1 day
            12 * 60 * 60 * 1000 +     // 12 hours
            30 * 60 * 1000 +          // 30 minutes
            45 * 1000; // 45 seconds
        assert_eq!(duration.to_millis(), expected);

        // Test with decimal values
        let duration = Duration::Expression(DurationExpression("P1.5D".to_string()));
        let expected = (1.5 * 24.0 * 60.0 * 60.0 * 1000.0) as u64; // 1.5 days
        assert_eq!(duration.to_millis(), expected);

        let duration = Duration::Expression(DurationExpression("PT2.5H".to_string()));
        let expected = (2.5 * 60.0 * 60.0 * 1000.0) as u64; // 2.5 hours
        assert_eq!(duration.to_millis(), expected);

        // Test with years, months and weeks
        let duration = Duration::Expression(DurationExpression("P1Y".to_string()));
        let expected = (365.25 * 24.0 * 60.0 * 60.0 * 1000.0) as u64; // 1 year (approximate)
        assert_eq!(duration.to_millis(), expected);

        let duration = Duration::Expression(DurationExpression("P1M".to_string()));
        let expected = (30.44 * 24.0 * 60.0 * 60.0 * 1000.0) as u64; // 1 month (approximate)
        assert_eq!(duration.to_millis(), expected);

        let duration = Duration::Expression(DurationExpression("P1W".to_string()));
        let expected = (7.0 * 24.0 * 60.0 * 60.0 * 1000.0) as u64; // 1 week
        assert_eq!(duration.to_millis(), expected);

        // Test complex expression
        let duration = Duration::Expression(DurationExpression("P1Y2M3W4DT5H6M7.8S".to_string()));
        let expected = (1.0 * 365.25 * 24.0 * 60.0 * 60.0 * 1000.0) as u64 + // 1 year
            (2.0 * 30.44 * 24.0 * 60.0 * 60.0 * 1000.0) as u64 +  // 2 months
            (3.0 * 7.0 * 24.0 * 60.0 * 60.0 * 1000.0) as u64 +    // 3 weeks
            (4.0 * 24.0 * 60.0 * 60.0 * 1000.0) as u64 +         // 4 days
            (5.0 * 60.0 * 60.0 * 1000.0) as u64 +                // 5 hours
            (6.0 * 60.0 * 1000.0) as u64 +                       // 6 minutes
            (7.8 * 1000.0) as u64; // 7.8 seconds
        assert_eq!(duration.to_millis(), expected);
    }
}
