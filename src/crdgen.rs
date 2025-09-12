mod types;

use kube::CustomResourceExt;
fn main() {
    print!(
        "{}",
        serde_yaml::to_string(&types::Redirect::crd()).unwrap()
    )
}
