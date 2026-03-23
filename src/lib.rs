//! Weather skill - Fetches current weather and a short forecast from Open-Meteo.

use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::fmt;

const INFO_LEVEL: u32 = 2;
const WARN_LEVEL: u32 = 3;
const ERROR_LEVEL: u32 = 4;
const EMPTY_JSON: &str = "{}";
// Weather API responses with a 3-day forecast can exceed 8 KB once current
// conditions, daily arrays, and JSON overhead are included, so we keep a 16 KB
// host buffer to avoid truncating valid responses.
const MAX_HOST_STRING_LEN: usize = 16 * 1024;
const FORECAST_DAYS: usize = 3;

#[derive(Debug, Deserialize)]
struct WeatherQuery {
    location: Option<String>,
    units: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestOptions {
    location: String,
    units: Units,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Units {
    Celsius,
    Fahrenheit,
}

#[derive(Debug, Clone, PartialEq)]
struct WeatherReport {
    location: String,
    units: Units,
    current: CurrentConditions,
    forecast: Vec<ForecastDay>,
}

#[derive(Debug, Clone, PartialEq)]
struct CurrentConditions {
    temperature: f64,
    humidity: u32,
    wind_speed: f64,
    weather_code: u32,
}

#[derive(Debug, Clone, PartialEq)]
struct ForecastDay {
    label: String,
    high: f64,
    low: f64,
    weather_code: u32,
}

#[derive(Debug, Deserialize)]
struct GeocodeResponse {
    results: Option<Vec<GeocodeResult>>,
}

#[derive(Debug, Deserialize)]
struct GeocodeResult {
    latitude: f64,
    longitude: f64,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedLocation {
    latitude: f64,
    longitude: f64,
}

#[derive(Debug, Deserialize)]
struct ForecastResponse {
    current: ApiCurrentWeather,
    daily: ApiDailyForecast,
}

#[derive(Debug, Deserialize)]
struct ApiCurrentWeather {
    temperature_2m: f64,
    relative_humidity_2m: u32,
    wind_speed_10m: f64,
    weather_code: u32,
}

#[derive(Debug, Deserialize)]
struct ApiDailyForecast {
    time: Vec<String>,
    weather_code: Vec<u32>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WeatherError {
    InvalidInput(String),
    RequestFailed(String),
    Api(String),
}

impl fmt::Display for WeatherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) | Self::RequestFailed(message) | Self::Api(message) => {
                formatter.write_str(message)
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ConditionInfo {
    emoji: &'static str,
    description: &'static str,
}

// Host API imports — linked to the "host_api_v1" WASM import module.
#[link(wasm_import_module = "host_api_v1")]
extern "C" {
    #[link_name = "log"]
    fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32);
    #[link_name = "get_input"]
    fn host_get_input() -> u32;
    #[link_name = "set_output"]
    fn host_set_output(text_ptr: *const u8, text_len: u32);
    #[link_name = "http_request"]
    fn host_http_request(
        method_ptr: *const u8,
        method_len: u32,
        url_ptr: *const u8,
        url_len: u32,
        headers_ptr: *const u8,
        headers_len: u32,
        body_ptr: *const u8,
        body_len: u32,
    ) -> u32;
}

/// Read a null-terminated string from a pointer in WASM linear memory.
///
/// # Safety
/// The caller must ensure `ptr` points to valid WASM linear memory.
unsafe fn read_host_string(ptr: u32) -> String {
    if ptr == 0 {
        return String::new();
    }

    let slice = core::slice::from_raw_parts(ptr as *const u8, MAX_HOST_STRING_LEN);
    let len = slice
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(MAX_HOST_STRING_LEN);
    String::from_utf8_lossy(&slice[..len]).into_owned()
}

fn log(level: u32, message: &str) {
    unsafe {
        host_log(level, message.as_ptr(), message.len() as u32);
    }
}

fn get_input() -> String {
    unsafe { read_host_string(host_get_input()) }
}

fn set_output(text: &str) {
    unsafe {
        host_set_output(text.as_ptr(), text.len() as u32);
    }
}

fn http_get(url: &str) -> Result<String, WeatherError> {
    let response = unsafe {
        read_host_string(host_http_request(
            b"GET".as_ptr(),
            3,
            url.as_ptr(),
            url.len() as u32,
            EMPTY_JSON.as_ptr(),
            EMPTY_JSON.len() as u32,
            b"".as_ptr(),
            0,
        ))
    };

    require_response_body(response)
}

fn require_response_body(response: String) -> Result<String, WeatherError> {
    if response.is_empty() {
        return Err(WeatherError::RequestFailed(
            "Weather service request failed.".to_string(),
        ));
    }

    Ok(response)
}

fn http_get_json<T: DeserializeOwned>(url: &str) -> Result<T, WeatherError> {
    let body = http_get(url)?;
    parse_json_response(&body)
}

fn parse_json_response<T: DeserializeOwned>(body: &str) -> Result<T, WeatherError> {
    serde_json::from_str(body)
        .map_err(|error| WeatherError::Api(format!("Failed to parse weather data: {error}")))
}

fn parse_query(input: &str) -> Result<RequestOptions, WeatherError> {
    let query: WeatherQuery = serde_json::from_str(input).map_err(|error| {
        WeatherError::InvalidInput(format!(
            "Invalid input JSON: {error}. Expected {{\"location\": \"Denver, CO\"}}."
        ))
    })?;

    let location = query.location.unwrap_or_default();
    let location = location.trim();
    if location.is_empty() {
        return Err(WeatherError::InvalidInput(
            "Please provide a location, like 'Denver, CO'.".to_string(),
        ));
    }

    Ok(RequestOptions {
        location: location.to_string(),
        units: parse_units(query.units.as_deref())?,
    })
}

fn parse_units(units: Option<&str>) -> Result<Units, WeatherError> {
    match units.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(Units::Fahrenheit),
        Some(value) if value.eq_ignore_ascii_case("celsius") => Ok(Units::Celsius),
        Some(value) if value.eq_ignore_ascii_case("fahrenheit") => Ok(Units::Fahrenheit),
        Some(value) => Err(WeatherError::InvalidInput(format!(
            "Unsupported units '{value}'. Use 'celsius' or 'fahrenheit'."
        ))),
    }
}

fn execute(input: &str) -> Result<String, WeatherError> {
    let options = parse_query(input)?;
    let report = fetch_weather(&options)?;
    Ok(format_report(&report))
}

fn fetch_weather(options: &RequestOptions) -> Result<WeatherReport, WeatherError> {
    let location = resolve_location(&options.location)?;
    let url = build_weather_url(&location, options.units);
    let forecast: ForecastResponse = http_get_json(&url)?;

    Ok(WeatherReport {
        location: options.location.clone(),
        units: options.units,
        current: CurrentConditions {
            temperature: forecast.current.temperature_2m,
            humidity: forecast.current.relative_humidity_2m,
            wind_speed: forecast.current.wind_speed_10m,
            weather_code: forecast.current.weather_code,
        },
        forecast: build_forecast(&forecast.daily)?,
    })
}

fn resolve_location(location: &str) -> Result<ResolvedLocation, WeatherError> {
    let url = build_geocode_url(location);
    let response: GeocodeResponse = http_get_json(&url)?;
    let result = response
        .results
        .and_then(|results| results.into_iter().next())
        .ok_or_else(|| WeatherError::Api(format!("No weather results found for '{location}'.")))?;

    Ok(ResolvedLocation {
        latitude: result.latitude,
        longitude: result.longitude,
    })
}

fn build_geocode_url(location: &str) -> String {
    format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1",
        encode_query_component(location)
    )
}

fn build_weather_url(location: &ResolvedLocation, units: Units) -> String {
    let mut url = format!(
        concat!(
            "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}",
            "&current=temperature_2m,relative_humidity_2m,wind_speed_10m,weather_code",
            "&daily=weather_code,temperature_2m_max,temperature_2m_min",
            "&timezone=auto&forecast_days=3"
        ),
        location.latitude, location.longitude
    );

    if matches!(units, Units::Fahrenheit) {
        url.push_str("&temperature_unit=fahrenheit&wind_speed_unit=mph");
    }

    url
}

fn encode_query_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn build_forecast(daily: &ApiDailyForecast) -> Result<Vec<ForecastDay>, WeatherError> {
    let total = forecast_entry_count(daily);
    if total == 0 {
        return Err(WeatherError::Api(
            "Weather forecast was missing daily data.".to_string(),
        ));
    }

    let mut days = Vec::with_capacity(total.min(FORECAST_DAYS));
    for index in 0..total.min(FORECAST_DAYS) {
        days.push(ForecastDay {
            label: weekday_label(&daily.time[index])?,
            high: daily.temperature_2m_max[index],
            low: daily.temperature_2m_min[index],
            weather_code: daily.weather_code[index],
        });
    }
    Ok(days)
}

fn forecast_entry_count(daily: &ApiDailyForecast) -> usize {
    let total = [
        daily.time.len(),
        daily.weather_code.len(),
        daily.temperature_2m_max.len(),
        daily.temperature_2m_min.len(),
    ]
    .into_iter()
    .min()
    .unwrap_or(0);

    if has_mismatched_daily_lengths(daily, total) {
        log(
            WARN_LEVEL,
            &format!(
                "Forecast arrays had mismatched lengths; truncating to {total} entries \
                 (time={}, weather_code={}, temperature_2m_max={}, temperature_2m_min={}).",
                daily.time.len(),
                daily.weather_code.len(),
                daily.temperature_2m_max.len(),
                daily.temperature_2m_min.len()
            ),
        );
    }

    total
}

fn has_mismatched_daily_lengths(daily: &ApiDailyForecast, expected: usize) -> bool {
    daily.time.len() != expected
        || daily.weather_code.len() != expected
        || daily.temperature_2m_max.len() != expected
        || daily.temperature_2m_min.len() != expected
}

fn weekday_label(date: &str) -> Result<String, WeatherError> {
    let (year, month, day) = parse_iso_date(date)?;
    let labels = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    Ok(labels[weekday_index(year, month, day)].to_string())
}

fn parse_iso_date(date: &str) -> Result<(i32, i32, i32), WeatherError> {
    let mut parts = date.split('-');
    let year = parse_date_part(parts.next(), "year", date)?;
    let month = parse_date_part(parts.next(), "month", date)?;
    let day = parse_date_part(parts.next(), "day", date)?;

    if parts.next().is_some() {
        return Err(invalid_forecast_date(date));
    }

    validate_date_parts(year, month, day, date)?;
    Ok((year, month, day))
}

fn parse_date_part(part: Option<&str>, label: &str, original: &str) -> Result<i32, WeatherError> {
    let value = part.ok_or_else(|| invalid_forecast_date(original))?;
    value
        .parse::<i32>()
        .map_err(|_| WeatherError::Api(format!("Invalid {label} in forecast date '{original}'.")))
}

fn validate_date_parts(
    year: i32,
    month: i32,
    day: i32,
    original: &str,
) -> Result<(), WeatherError> {
    if !(1..=12).contains(&month) {
        return Err(invalid_forecast_date(original));
    }

    let max_day = max_day_in_month(year, month);
    if !(1..=max_day).contains(&day) {
        return Err(invalid_forecast_date(original));
    }

    Ok(())
}

fn max_day_in_month(year: i32, month: i32) -> i32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn invalid_forecast_date(date: &str) -> WeatherError {
    WeatherError::Api(format!("Invalid forecast date '{date}'."))
}

fn weekday_index(year: i32, month: i32, day: i32) -> usize {
    let offsets = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let adjusted_year = if month < 3 { year - 1 } else { year };
    let weekday = adjusted_year + adjusted_year / 4 - adjusted_year / 100
        + adjusted_year / 400
        + offsets[(month - 1) as usize]
        + day;
    (weekday.rem_euclid(7)) as usize
}

fn format_report(report: &WeatherReport) -> String {
    let current_condition = weather_condition(report.current.weather_code);
    let mut lines = vec![
        format!("🌤️ Weather for {}", report.location),
        String::new(),
        format!(
            "Current: {}, {}",
            current_temperature_text(report.current.temperature, report.units),
            current_condition.description
        ),
        format!(
            "Humidity: {}% | Wind: {:.0} {}",
            report.current.humidity,
            report.current.wind_speed,
            report.units.wind_unit()
        ),
        String::new(),
        "📅 3-Day Forecast:".to_string(),
    ];

    for day in &report.forecast {
        let condition = weather_condition(day.weather_code);
        lines.push(format!(
            "  {}: {} {} / {} — {}",
            day.label,
            condition.emoji,
            temperature_text(day.high, report.units),
            temperature_text(day.low, report.units),
            condition.description
        ));
    }

    lines.join("\n")
}

fn current_temperature_text(value: f64, units: Units) -> String {
    let secondary_units = units.other();
    let secondary_value = convert_temperature(value, units, secondary_units);
    format!(
        "{} ({})",
        temperature_text(value, units),
        temperature_text(secondary_value, secondary_units)
    )
}

fn temperature_text(value: f64, units: Units) -> String {
    format!("{}{symbol}", value.round() as i32, symbol = units.symbol())
}

fn convert_temperature(value: f64, from: Units, to: Units) -> f64 {
    match (from, to) {
        (Units::Celsius, Units::Fahrenheit) => (value * 9.0 / 5.0) + 32.0,
        (Units::Fahrenheit, Units::Celsius) => (value - 32.0) * 5.0 / 9.0,
        _ => value,
    }
}

fn weather_condition(code: u32) -> ConditionInfo {
    let (emoji, description) = match code {
        0 => ("☀️", "Clear Sky"),
        1..=3 => ("⛅", "Partly Cloudy"),
        45 | 48 => ("🌫️", "Fog"),
        51..=55 => ("🌦️", "Drizzle"),
        56..=57 => ("🌧️", "Freezing Drizzle"),
        61..=65 => ("🌧️", "Rain"),
        66..=67 => ("🌧️", "Freezing Rain"),
        71..=75 => ("❄️", "Snow"),
        77 => ("❄️", "Snow Grains"),
        80..=82 => ("🌦️", "Rain Showers"),
        85..=86 => ("🌨️", "Snow Showers"),
        95 => ("⛈️", "Thunderstorm"),
        96 | 99 => ("⛈️", "Thunderstorm With Hail"),
        _ => ("🌤️", "Unknown Conditions"),
    };

    ConditionInfo { emoji, description }
}

fn error_output(error: &WeatherError) -> String {
    format!("{{\"error\": {}}}", json_string(&error.to_string()))
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"Internal serialization error.\"".to_string())
}

impl Units {
    fn symbol(self) -> &'static str {
        match self {
            Self::Celsius => "°C",
            Self::Fahrenheit => "°F",
        }
    }

    fn wind_unit(self) -> &'static str {
        match self {
            Self::Celsius => "km/h",
            Self::Fahrenheit => "mph",
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Celsius => Self::Fahrenheit,
            Self::Fahrenheit => Self::Celsius,
        }
    }
}

#[no_mangle]
pub extern "C" fn run() {
    log(INFO_LEVEL, "Weather skill starting");
    let input = get_input();

    match execute(&input) {
        Ok(output) => set_output(&output),
        Err(error) => {
            log(ERROR_LEVEL, &error.to_string());
            set_output(&error_output(&error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_code_mapping_matches_wmo_groups() {
        assert_eq!(weather_condition(0).description, "Clear Sky");
        assert_eq!(weather_condition(2).description, "Partly Cloudy");
        assert_eq!(weather_condition(48).description, "Fog");
        assert_eq!(weather_condition(53).description, "Drizzle");
        assert_eq!(weather_condition(56).description, "Freezing Drizzle");
        assert_eq!(weather_condition(63).description, "Rain");
        assert_eq!(weather_condition(66).description, "Freezing Rain");
        assert_eq!(weather_condition(73).description, "Snow");
        assert_eq!(weather_condition(77).description, "Snow Grains");
        assert_eq!(weather_condition(81).description, "Rain Showers");
        assert_eq!(weather_condition(85).description, "Snow Showers");
        assert_eq!(weather_condition(95).description, "Thunderstorm");
        assert_eq!(weather_condition(96).description, "Thunderstorm With Hail");
    }

    #[test]
    fn temperature_unit_conversion_handles_both_directions() {
        let fahrenheit = convert_temperature(22.0, Units::Celsius, Units::Fahrenheit);
        let celsius = convert_temperature(72.0, Units::Fahrenheit, Units::Celsius);

        assert!((fahrenheit - 71.6).abs() < 0.01);
        assert!((celsius - 22.222_222).abs() < 0.01);
    }

    #[test]
    fn temperature_unit_conversion_returns_identity_for_same_units() {
        assert_eq!(
            convert_temperature(22.0, Units::Celsius, Units::Celsius),
            22.0
        );
        assert_eq!(
            convert_temperature(72.0, Units::Fahrenheit, Units::Fahrenheit),
            72.0
        );
    }

    #[test]
    fn input_json_parsing_defaults_to_fahrenheit() {
        let query = parse_query(r#"{"location":"Tokyo"}"#).expect("query should parse");

        assert_eq!(query.location, "Tokyo");
        assert_eq!(query.units, Units::Fahrenheit);
    }

    #[test]
    fn input_json_parsing_accepts_explicit_units() {
        let query =
            parse_query(r#"{"location":"London","units":"celsius"}"#).expect("query should parse");

        assert_eq!(query.location, "London");
        assert_eq!(query.units, Units::Celsius);
    }

    #[test]
    fn missing_location_returns_friendly_error() {
        let error = parse_query(r#"{"units":"fahrenheit"}"#).expect_err("location is required");
        assert_eq!(
            error.to_string(),
            "Please provide a location, like 'Denver, CO'."
        );
    }

    #[test]
    fn invalid_units_are_rejected() {
        let error = parse_units(Some("kelvin")).expect_err("units should be rejected");
        assert_eq!(
            error.to_string(),
            "Unsupported units 'kelvin'. Use 'celsius' or 'fahrenheit'."
        );
    }

    #[test]
    fn malformed_query_json_returns_friendly_error() {
        let error = parse_query("not json").expect_err("json should be rejected");
        assert!(error
            .to_string()
            .starts_with("Invalid input JSON: expected ident at line 1 column 2."));
    }

    #[test]
    fn response_formatting_matches_expected_layout() {
        let output = format_report(&sample_weather_report());
        let expected = concat!(
            "🌤️ Weather for Denver, CO\n\n",
            "Current: 72°F (22°C), Partly Cloudy\n",
            "Humidity: 35% | Wind: 12 mph\n\n",
            "📅 3-Day Forecast:\n",
            "  Mon: ☀️ 75°F / 48°F — Clear Sky\n",
            "  Tue: 🌧️ 62°F / 41°F — Rain\n",
            "  Wed: ⛅ 68°F / 45°F — Partly Cloudy"
        );

        assert_eq!(output, expected);
    }

    #[test]
    fn current_temperature_text_includes_secondary_units() {
        assert_eq!(
            current_temperature_text(72.0, Units::Fahrenheit),
            "72°F (22°C)"
        );
    }

    #[test]
    fn encode_query_component_handles_spaces_unicode_and_symbols() {
        assert_eq!(
            encode_query_component("São Paulo & Co"),
            "S%C3%A3o%20Paulo%20%26%20Co"
        );
    }

    #[test]
    fn weekday_label_uses_iso_dates() {
        let label = weekday_label("2026-03-09").expect("date should parse");
        assert_eq!(label, "Mon");
    }

    #[test]
    fn weekday_label_handles_known_historical_date() {
        let label = weekday_label("2000-01-01").expect("date should parse");
        assert_eq!(label, "Sat");
    }

    #[test]
    fn parse_iso_date_rejects_invalid_dates() {
        let malformed = parse_iso_date("not-a-date").expect_err("date should be rejected");
        assert_eq!(
            malformed.to_string(),
            "Invalid year in forecast date 'not-a-date'."
        );

        for date in [
            "2026-00-15",
            "2026-13-01",
            "2026-02-29",
            "2024-02-30",
            "2026-04-31",
            "2026-03-09-extra",
        ] {
            let error = parse_iso_date(date).expect_err("date should be rejected");
            assert_eq!(
                error.to_string(),
                format!("Invalid forecast date '{date}'.")
            );
        }
    }

    #[test]
    fn empty_http_response_returns_request_failed_error() {
        let error = require_response_body(String::new()).expect_err("empty response should fail");
        assert_eq!(error.to_string(), "Weather service request failed.");
    }

    #[test]
    fn invalid_json_response_returns_parse_error() {
        let error = parse_json_response::<GeocodeResponse>("not json")
            .expect_err("json response should fail");
        assert!(error
            .to_string()
            .starts_with("Failed to parse weather data: expected ident at line 1 column 2"));
    }

    #[test]
    fn build_forecast_rejects_empty_daily_data() {
        let error = build_forecast(&sample_daily_forecast(vec![], vec![], vec![], vec![]))
            .expect_err("empty forecast should fail");
        assert_eq!(
            error.to_string(),
            "Weather forecast was missing daily data."
        );
    }

    #[test]
    fn build_forecast_truncates_to_available_days() {
        let forecast = build_forecast(&sample_daily_forecast(
            vec!["2026-03-09", "2026-03-10"],
            vec![0, 61],
            vec![75.0, 62.0],
            vec![48.0, 41.0],
        ))
        .expect("forecast should build");

        assert_eq!(forecast.len(), 2);
        assert_eq!(forecast[0].label, "Mon");
        assert_eq!(forecast[1].label, "Tue");
    }

    #[test]
    fn forecast_entry_count_uses_shortest_array_length() {
        let total = forecast_entry_count(&sample_daily_forecast(
            vec!["2026-03-09", "2026-03-10", "2026-03-11"],
            vec![0, 61],
            vec![75.0, 62.0, 68.0],
            vec![48.0, 41.0, 45.0],
        ));

        assert_eq!(total, 2);
    }

    #[test]
    fn error_output_matches_json_contract() {
        let output = error_output(&WeatherError::Api(
            "Weather service request failed.".to_string(),
        ));
        assert_eq!(output, r#"{"error": "Weather service request failed."}"#);
    }

    fn sample_weather_report() -> WeatherReport {
        WeatherReport {
            location: "Denver, CO".to_string(),
            units: Units::Fahrenheit,
            current: CurrentConditions {
                temperature: 72.0,
                humidity: 35,
                wind_speed: 12.0,
                weather_code: 2,
            },
            forecast: vec![
                ForecastDay {
                    label: "Mon".to_string(),
                    high: 75.0,
                    low: 48.0,
                    weather_code: 0,
                },
                ForecastDay {
                    label: "Tue".to_string(),
                    high: 62.0,
                    low: 41.0,
                    weather_code: 61,
                },
                ForecastDay {
                    label: "Wed".to_string(),
                    high: 68.0,
                    low: 45.0,
                    weather_code: 2,
                },
            ],
        }
    }

    fn sample_daily_forecast(
        dates: Vec<&str>,
        weather_codes: Vec<u32>,
        highs: Vec<f64>,
        lows: Vec<f64>,
    ) -> ApiDailyForecast {
        ApiDailyForecast {
            time: dates.into_iter().map(str::to_string).collect(),
            weather_code: weather_codes,
            temperature_2m_max: highs,
            temperature_2m_min: lows,
        }
    }
}
