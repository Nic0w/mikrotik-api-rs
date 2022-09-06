use clap::Parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    #[clap(short = 'A', long, help = "<HOST>:<PORT>")]
    pub address: String,

    #[clap(short = 'L', long)]
    pub login: String,

    #[clap(short = 'P', long)]
    pub password: String,

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
}
