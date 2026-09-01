//! Проверка запуска NanoTrack-моделей через tract с разными уровнями
//! оптимизации. Запуск: cargo run -p nano-track --example nano_smoke --release

use std::time::Instant;
use tract_onnx::prelude::*;

type Model = RunnableModel<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

fn load(path: &str, optimize: bool) -> anyhow::Result<Model> {
    let typed = tract_onnx::onnx()
        .model_for_path(path)?
        .into_typed()?;
    let typed = if optimize {
        typed.into_optimized()?
    } else {
        typed
    };
    Ok(typed.into_runnable()?)
}

fn smoke(path: &str, optimize: bool, size: usize) -> anyhow::Result<()> {
    let t0 = Instant::now();
    let model = load(path, optimize)?;
    println!("[{path}] optimize={optimize}: загрузка {} мс", t0.elapsed().as_millis());
    let input = Tensor::from_shape(&[1usize, 3, size, size], &vec![0.5f32; 3 * size * size])?;
    let t1 = Instant::now();
    let out = model.run(tvec!(TValue::from(input)))?;
    println!(
        "  инференс {} мс, выходов: {}, первый: {:?}",
        t1.elapsed().as_millis(),
        out.len(),
        out[0].shape()
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    for (path, size) in [
        ("models/nanotrack_backbone_127.onnx", 127usize),
        ("models/nanotrack_backbone_sim.onnx", 255usize),
    ] {
        for optimize in [true, false] {
            if let Err(e) = smoke(path, optimize, size) {
                println!("[{path}] optimize={optimize}: ОШИБКА: {e:#}");
            }
        }
    }
    // Голова: два входа 8x8 и 16x16.
    let head_path = "models/nanotrack_head_sim.onnx";
    for optimize in [true, false] {
        let t0 = Instant::now();
        let res: anyhow::Result<Model> = (|| {
            let typed = tract_onnx::onnx()
                .model_for_path(head_path)?
                .into_typed()?;
            let typed = if optimize { typed.into_optimized()? } else { typed };
            Ok(typed.into_runnable()?)
        })();
        match res {
            Ok(model) => {
                let t1 = Tensor::from_shape(&[1usize, 48, 8, 8], &vec![0.1f32; 48 * 64])?;
                let t2 = Tensor::from_shape(&[1usize, 48, 16, 16], &vec![0.1f32; 48 * 256])?;
                let ta = Instant::now();
                let out = model.run(tvec!(TValue::from(t1), TValue::from(t2)))?;
                println!(
                    "[head] optimize={optimize}: load {} мс, инференс {} мс, выходы: {:?} + {:?}",
                    t0.elapsed().as_millis(),
                    ta.elapsed().as_millis(),
                    out[0].shape(),
                    out[1].shape()
                );
            }
            Err(e) => println!("[head] optimize={optimize}: ОШИБКА: {e:#}"),
        }
    }
    Ok(())
}
