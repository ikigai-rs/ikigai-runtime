//! A stock component: `acme:calc/calc`, with `acme:calc/lookup` imported.

use std::cell::Cell;

wit_bindgen::generate!({
    path: "wit",
    world: "acme:calc/calc-app",
});

use exports::acme::calc::calc::{Color, Guest as CalcGuest, GuestCounter, Stats};

struct Calc;

struct Counter(Cell<u32>);

impl GuestCounter for Counter {
    fn new() -> Self {
        Counter(Cell::new(0))
    }

    fn bump(&self) -> u32 {
        self.0.set(self.0.get() + 1);
        self.0.get()
    }
}

impl CalcGuest for Calc {
    type Counter = Counter;

    fn add(a: i32, b: i32) -> i32 {
        a.wrapping_add(b)
    }

    fn greet(name: String) -> String {
        format!("hello, {name}")
    }

    fn shout(body: Vec<u8>) -> Vec<u8> {
        body.to_ascii_uppercase()
    }

    fn summarize(xs: Vec<f64>) -> Stats {
        let min = xs.iter().copied().fold(f64::INFINITY, f64::min);
        let max = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mean = xs.iter().sum::<f64>() / xs.len().max(1) as f64;
        Stats { min, max, mean }
    }

    fn parse(text: String) -> Result<u32, String> {
        text.trim()
            .parse()
            .map_err(|_| format!("`{text}` is not a number"))
    }

    fn half(n: u32) -> Option<u32> {
        n.is_multiple_of(2).then_some(n / 2)
    }

    fn paint(c: Color, shade: Option<u8>) -> String {
        let name = match c {
            Color::Red => "red",
            Color::Green => "green",
            Color::Blue => "blue",
        };
        match shade {
            Some(s) => format!("{name}/{s}"),
            None => name.to_string(),
        }
    }

    fn locate(place: String) -> String {
        format!("{place} is at {}", acme::calc::lookup::find(&place))
    }

    fn save(text: String) -> u32 {
        text.len() as u32
    }

    fn spin() -> u64 {
        let mut i: u64 = 0;
        loop {
            i = i.wrapping_add(1);
            core::hint::black_box(i);
        }
    }

    fn open() -> exports::acme::calc::calc::Counter {
        exports::acme::calc::calc::Counter::new(Counter::new())
    }
}

impl Guest for Calc {
    fn version() -> String {
        "1.0.0".to_string()
    }
}

export!(Calc);
