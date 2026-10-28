use std::collections::HashSet;

use crate::{FunError, RouteConfig};

#[derive(Clone, Debug)]
pub struct RouteMatch {
    pub function: String,
    pub path_parameters: Vec<(String, String)>,
    pub allowed_methods: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct RouteTable {
    routes: Vec<CompiledRoute>,
}

#[derive(Clone, Debug)]
struct CompiledRoute {
    segments: Vec<Segment>,
    function: String,
    methods: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Parameter(String),
}

impl RouteTable {
    pub fn new(routes: &[RouteConfig]) -> Result<Self, FunError> {
        let mut compiled: Vec<CompiledRoute> = Vec::with_capacity(routes.len());
        for route in routes {
            let segments = parse_path(&route.path)?;
            let methods = if route.methods.is_empty() {
                None
            } else {
                let mut normalized = Vec::new();
                for method in &route.methods {
                    if method.is_empty()
                        || !method
                            .bytes()
                            .all(|byte| byte.is_ascii_uppercase() || byte == b'-')
                    {
                        return Err(FunError::Config(format!(
                            "invalid HTTP method `{method}` in route `{}`",
                            route.path
                        )));
                    }
                    if !normalized.contains(method) {
                        normalized.push(method.clone());
                    }
                }
                Some(normalized)
            };
            for existing in &compiled {
                if paths_overlap(&existing.segments, &segments)
                    && methods_overlap(existing.methods.as_deref(), methods.as_deref())
                {
                    return Err(FunError::Config(format!(
                        "route `{}` overlaps another route for at least one HTTP method",
                        route.path
                    )));
                }
            }
            compiled.push(CompiledRoute {
                segments,
                function: route.function.clone(),
                methods,
            });
        }
        Ok(Self { routes: compiled })
    }

    pub fn resolve(&self, path: &str, method: &str) -> Option<RouteMatch> {
        let actual: Vec<&str> = split_path(path);
        let mut path_match: Option<RouteMatch> = None;
        for route in &self.routes {
            if actual.len() != route.segments.len() {
                continue;
            }
            let mut parameters = Vec::new();
            let matches =
                route
                    .segments
                    .iter()
                    .zip(&actual)
                    .all(|(expected, value)| match expected {
                        Segment::Literal(literal) => literal == value,
                        Segment::Parameter(name) => {
                            parameters.push((name.clone(), (*value).to_owned()));
                            true
                        }
                    });
            if !matches {
                continue;
            }
            if route
                .methods
                .as_ref()
                .is_none_or(|methods| methods.iter().any(|allowed| allowed == method))
            {
                return Some(RouteMatch {
                    function: route.function.clone(),
                    path_parameters: parameters,
                    allowed_methods: route.methods.clone(),
                });
            }
            match &mut path_match {
                Some(found) => {
                    if let (Some(allowed), Some(route_methods)) =
                        (&mut found.allowed_methods, &route.methods)
                    {
                        for method in route_methods {
                            if !allowed.contains(method) {
                                allowed.push(method.clone());
                            }
                        }
                    }
                }
                None => {
                    path_match = Some(RouteMatch {
                        function: route.function.clone(),
                        path_parameters: parameters,
                        allowed_methods: route.methods.clone(),
                    });
                }
            }
        }
        path_match
    }
}

fn parse_path(path: &str) -> Result<Vec<Segment>, FunError> {
    if !path.starts_with('/') || path.contains('?') || path.contains('#') {
        return Err(FunError::Config(format!(
            "route path `{path}` must be an absolute path without query or fragment"
        )));
    }
    let mut parameter_names = HashSet::new();
    let segments = split_path(path)
        .into_iter()
        .map(|part| {
            if let Some(name) = part
                .strip_prefix('{')
                .and_then(|value| value.strip_suffix('}'))
            {
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    return Err(FunError::Config(format!(
                        "invalid path parameter `{part}` in route `{path}`"
                    )));
                }
                if !parameter_names.insert(name.to_owned()) {
                    return Err(FunError::Config(format!(
                        "route `{path}` repeats path parameter `{name}`"
                    )));
                }
                Ok(Segment::Parameter(name.to_owned()))
            } else if part.contains('{') || part.contains('}') || part.is_empty() {
                Err(FunError::Config(format!("invalid route path `{path}`")))
            } else {
                Ok(Segment::Literal(part.to_owned()))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(segments)
}

fn split_path(path: &str) -> Vec<&str> {
    if path == "/" {
        Vec::new()
    } else {
        path.trim_start_matches('/')
            .trim_end_matches('/')
            .split('/')
            .collect()
    }
}

fn paths_overlap(left: &[Segment], right: &[Segment]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| match (a, b) {
            (Segment::Literal(left), Segment::Literal(right)) => left == right,
            _ => true,
        })
}

fn methods_overlap(left: Option<&[String]>, right: Option<&[String]>) -> bool {
    match (left, right) {
        (None, _) | (_, None) => true,
        (Some(left), Some(right)) => left.iter().any(|method| right.contains(method)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(path: &str, function: &str, methods: &[&str]) -> RouteConfig {
        RouteConfig {
            path: path.into(),
            function: function.into(),
            methods: methods.iter().map(|method| (*method).into()).collect(),
        }
    }

    #[test]
    fn matches_parameter_and_method() {
        let routes = RouteTable::new(&[route("/users/{id}", "user", &["GET"])]).unwrap();
        let found = routes.resolve("/users/42", "GET").unwrap();
        assert_eq!(found.function, "user");
        assert_eq!(found.path_parameters, vec![("id".into(), "42".into())]);
        assert_eq!(
            routes.resolve("/users/42", "POST").unwrap().allowed_methods,
            Some(vec!["GET".into()])
        );
    }

    #[test]
    fn rejects_overlapping_routes_for_common_methods() {
        let routes = [
            route("/users/{id}", "user", &[]),
            route("/users/me", "me", &["GET"]),
        ];
        assert!(RouteTable::new(&routes).is_err());
    }

    #[test]
    fn permits_disjoint_methods_for_same_path() {
        assert!(RouteTable::new(&[
            route("/hello", "get", &["GET"]),
            route("/hello", "post", &["POST"]),
        ])
        .is_ok());
    }

    #[test]
    fn combines_allowed_methods_for_overlapping_paths() {
        let routes = RouteTable::new(&[
            route("/users/{id}", "get-user", &["GET"]),
            route("/users/me", "post-me", &["POST"]),
        ])
        .unwrap();
        let found = routes.resolve("/users/me", "DELETE").unwrap();
        assert_eq!(
            found.allowed_methods,
            Some(vec!["GET".into(), "POST".into()])
        );
    }

    #[test]
    fn rejects_repeated_parameter_names() {
        assert!(RouteTable::new(&[route("/users/{id}/orders/{id}", "orders", &[])]).is_err());
    }
}
