use crate::StateBox;

pub struct Flag {
    pub short: char,
    pub long: String,
    pub about: String,
    pub consumer: bool,
    pub breakpoint: bool,
    pub run_func: fn(parent: &mut StateBox, flag: Option<&String>),
}

impl PartialEq for Flag {
    fn eq(
        &self,
        Flag {
            short: _,
            long: _,
            about: _,
            consumer: _,
            breakpoint: _,
            run_func: _,
        }: &Self,
    ) -> bool {
        false
    }
}

impl Flag {
    pub fn help(&self) -> String {
        let mut help = String::new();
        help.push_str(&format!("-{}, --{}\t{}", self.short, self.long, self.about));
        help
    }
}
