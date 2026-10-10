// userland/utilities/hello.rs
#![no_std]
#![no_main]

use imagineos_rt::{main, println};

fn user_main() -> Result<(), i32> {
    // A escrita pode falhar (console indisponível); o `let _` mantém o
    // comportamento intencional de não abortar por isso.
    let _ = println!("Hello from ImagineOS!");
    let _ = println!("Este é um programa limpo rodando na ABI v1!");

    let x = 10;
    let y = 20;
    let _ = println!("x = {}, y = {}", x, y);
    let _ = println!("x + y = {}", x + y);
    
    Ok(())
}

// Expande o _start, valida a ABI_VERSION e chama user_main()
main!(user_main);