use pyo3::prelude::*;

#[pymodule]
fn smartkey_py(_py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // PySmartKeyEngine will be added in Task 7
    Ok(())
}
