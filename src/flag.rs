use crate::StateBox;

pub struct Flag {
    pub short: Option<char>,
    pub long: String,
    pub about: String,
    pub consumer: bool,
    pub breakpoint: bool,
    pub run_func: fn(parent: &mut StateBox, flag: Option<&String>),
}

impl PartialEq for Flag {
    // Superfluous PartialEq implementation to allow for struct field equality checks.
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
        let short = if let Some(short) = self.short {
            format!("-{short},")
        } else {
            String::from("   ")
        };
        help.push_str(&format!("{} --{}\t{}", short, self.long, self.about));
        help
    }
}
