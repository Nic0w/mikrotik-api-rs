use clap::Parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    #[clap(short = 'A', long, help = "<HOST>:<PORT>")]
    pub address: String,

    #[clap(short = 'L', long)]
    pub login: String,

    #[clap(short = 'P', long)]
    pub password: Option<String>,

    #[clap(subcommand)]
    pub command: Command,
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    Identify {
        #[clap(long, help = "show target's ressources as well.")]
        full: bool,
    },

    ActiveUsers,

    Custom {
        #[clap(long, help = "run one-off command")]
        one_off: bool,

        #[clap(long, help = "run array-list command")]
        array_list: bool,

        #[clap(long, help = "run listen command")]
        listen: bool,

        #[clap(short, long, help = "set .proplist")]
        proplist: Option<String>,

        command: String,
    },
}
