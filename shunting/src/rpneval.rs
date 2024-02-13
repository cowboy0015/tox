use crate::parser::RPNExpr;
use lexers::MathToken;
use std::collections::HashMap;
use std::rc::Rc;
use std::cell::RefCell;


pub trait RandomVariable {
    fn eval(&self) -> f64;
}

#[derive(Clone)]
pub enum MathOp {
    Number(f64),
    Dynamic(Rc<dyn Fn() -> Result<f64, String>>),
}

impl RandomVariable for MathOp {
    fn eval(&self) -> f64 {
        match self {
            MathOp::Number(n) => *n,
            MathOp::Dynamic(f) => f().unwrap(),
        }
    }
}

#[derive(Debug)]
pub struct Histogram<const BUCKETS: usize> {
    pub buckets: [u32; BUCKETS],
    pub min: f64,
    pub max: f64,
}

impl MathOp {
    pub fn histogram<const BUCKETS: usize>(&self, samples: usize) -> Histogram<BUCKETS> {
        // collect samples from random variable
        let data: Vec<_> = (0..samples).map(|_| self.eval()).collect();
        // extract info from data to build histogram
        let (min, max) = data.iter().fold((f64::MAX, f64::MIN), |(min, max), &x| {
            (min.min(x), max.max(x))
        });
        let bucket_size = (max + 1.0e-10 - min) / BUCKETS as f64;
        // map samples to histogram buckets
        let mut histogram = Histogram{buckets: [0; BUCKETS], min, max};
        for bucket in data.into_iter().map(|x| (x - min) / bucket_size) {
            histogram.buckets[bucket as usize] += 1;
        }
        histogram
    }
}

pub struct MathContext(Rc<RefCell<HashMap<String, MathOp>>>);

impl MathContext {
    pub fn new() -> MathContext {
        use std::f64::consts;
        let mut cx = HashMap::new();
        cx.insert("pi".to_string(), MathOp::Number(consts::PI));
        cx.insert("e".to_string(), MathOp::Number(consts::E));
        MathContext(Rc::new(RefCell::new(cx)))
    }

    pub fn setvar(&self, name: &str, value: MathOp) {
        self.0.borrow_mut().insert(name.to_string(), value);
    }

    pub fn eval(&self, rpn: &RPNExpr) -> Result<f64, String> {
        let mut operands = Vec::new();

        for token in &rpn.0 {
            match token {
                MathToken::Number(num) => operands.push(*num),
                MathToken::Variable(ref v) => operands.push(
                    match self.0.borrow().get(v) {
                        Some(mathop) => mathop.eval(),
                        None => return Err(format!("Unknown Variable: {}", v)),
                    }
                ),
                MathToken::BOp(op) => {
                    let rhs = operands.pop().ok_or("Missing operands")?;
                    let lhs = operands.pop().ok_or("Missing operands")?;
                    operands.push(match &op[..] {
                        "+" => lhs + rhs,
                        "-" => lhs - rhs,
                        "*" => lhs * rhs,
                        "/" => lhs / rhs,
                        "%" => lhs % rhs,
                        "^" | "**" => lhs.powf(rhs),
                        _ => return Err(format!("Unknown BOp: {}", op)),
                    });
                }
                MathToken::UOp(op) => {
                    let arg = operands.pop().ok_or("Missing operands")?;
                    operands.push(match &op[..] {
                        "-" => -arg,
                        "!" => libm::tgamma(arg + 1.0),
                        _ => return Err(format!("Unknown UOp: {}", op)),
                    });
                }
                MathToken::Function(fname, arity) => {
                    if *arity > operands.len() {
                        return Err(format!("Missing args for function {}", fname));
                    }
                    let args: Vec<_> = operands.split_off(operands.len() - arity);
                    operands.push(
                        eval_fn(fname, &args)?
                    );
                }
                _ => return Err(format!("Unexpected token for RPN eval: {:?}", token)),
            }
        }
        operands.pop().ok_or(format!("Failed to eval RPN: {:?}", rpn))
    }

    pub fn compile(&self, rpn: &RPNExpr) -> Result<MathOp, String> {
        let mut stack = Vec::new();
        for token in &rpn.0 {
            match token {
                MathToken::Number(n) => stack.push(MathOp::Number(*n)),
                MathToken::Variable(v) => stack.push(
                    self.0.borrow().get(v).ok_or(format!("Unknown variable: {}", v))?.clone()),
                MathToken::BOp(op) => {
                    let rhs = stack.pop().ok_or(format!("Missing operands for {}", op))?;
                    let lhs = stack.pop().ok_or(format!("Missing operands for {}", op))?;
                    let dynamic = !(
                        matches!(rhs, MathOp::Number(_)) && matches!(lhs, MathOp::Number(_)));
                    let op = op.clone();
                    let eval = move || {
                        Ok(match op.as_str() {
                            "+" => lhs.eval() + rhs.eval(),
                            "-" => lhs.eval() - rhs.eval(),
                            "*" => lhs.eval() * rhs.eval(),
                            "/" => lhs.eval() / rhs.eval(),
                            "%" => lhs.eval() % rhs.eval(),
                            "^" | "**" => lhs.eval().powf(rhs.eval()),
                            _ => return Err(format!("Unknown BOp: {}", op)),
                        })
                    };
                    stack.push(if dynamic {
                        MathOp::Dynamic(Rc::new(eval))
                    } else {
                        MathOp::Number(eval()?)
                    });
                }
                MathToken::UOp(op) => {
                    let arg = stack.pop().ok_or(format!("Missing operands for {}", op))?;
                    let dynamic = !matches!(arg, MathOp::Number(_));
                    let op = op.clone();
                    let eval = move || {
                        Ok(match op.as_str() {
                            "-" => -arg.eval(),
                            "!" => libm::tgamma(arg.eval() + 1.0),
                            _ => return Err(format!("Unknown UOp: {}", op)),
                        })
                    };
                    stack.push(if dynamic {
                        MathOp::Dynamic(Rc::new(eval))
                    } else {
                        MathOp::Number(eval()?)
                    });
                }
                MathToken::Function(fname, arity) => {
                    if *arity > stack.len() {
                        return Err(format!("Missing args for {}", fname));
                    }
                    let args: Vec<_> = stack.split_off(stack.len() - arity);
                    let dynamic = !args.iter().all(|arg| matches!(arg, MathOp::Number(_)));
                    let fname = fname.clone();
                    let eval = move || -> Result<MathOp, String> {
                        let args: Vec<_> = args.iter().map(|v| v.eval()).collect();
                        Ok(
                            MathOp::Number(eval_fn(&fname, &args)?)
                        )
                    };
                    stack.push(if dynamic {
                        MathOp::Dynamic(Rc::new(move || eval().map(|v| v.eval())))
                    } else {
                        eval()?
                    });
                }
                _ => return Err(format!("Unexpected token for RPN compile: {:?}", token)),
            }
        }
        assert_eq!(stack.len(), 1);
        Ok(stack.pop().ok_or("Failed to compile RPNExpr")?)
    }
}

fn eval_fn(fname: &str, args: &[f64]) -> Result<f64, String> {
    Ok(match fname {
        "abs" if args.len() == 1 => args[0].abs(),
        "atan2" if args.len() == 2 => args[0].atan2(args[1]),
        "cos" if args.len() == 1 => args[0].cos(),
        "log" if args.len() == 1 => args[0].log10(),
        "max" if !args.is_empty() => args.iter().fold(args[0], |a, &b| a.max(b)),
        "min" if !args.is_empty() => args.iter().fold(args[0], |a, &b| a.min(b)),
        // Order not important
        "nCr" if args.len() == 2 => funcs::combinations(args[0], args[1])?,
        "nMCr" if args.len() == 2 => funcs::multicombinations(args[0], args[1])?,
        // Order is important
        "nMPr" if args.len() == 2 => args[0].powf(args[1]),
        "nPr" if args.len() == 2 => funcs::permutations(args[0], args[1])?,
        "sin" if args.len() == 1 => args[0].sin(),
        _ => return Err(format!("Unknown Function: {} with {} args", fname, args.len()))
    })
}

mod funcs {
    pub fn combinations(n: f64, r: f64) -> Result<f64, String> {
        use libm::tgamma;
        Ok(tgamma(n + 1.0) / tgamma(r + 1.0) / tgamma(n - r + 1.0))
    }

    pub fn multicombinations(n: f64, r: f64) -> Result<f64, String> {
        use libm::tgamma;
        Ok(tgamma(n + r) / tgamma(r + 1.0) / tgamma(n))
    }

    pub fn permutations(n: f64, r: f64) -> Result<f64, String> {
        use libm::tgamma;
        Ok(tgamma(n + 1.0) / tgamma(n - r + 1.0))
    }
}
