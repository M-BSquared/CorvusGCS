use super::*;

#[test]
fn the_view_box_is_the_ground_the_map_shows() {
    // At zoom 16 a pixel is about 1.6 m at 48 degrees north.
    let px = 156_543.033_9 * 48.08f64.to_radians().cos() / 65_536.0;
    let b = view_box((11.64, 48.08), 16.0, (1000.0, 700.0));
    let width_m = (b[2] - b[0]) * 111_320.0 * 48.08f64.to_radians().cos();
    let height_m = (b[3] - b[1]) * 111_320.0;
    assert!((width_m - 1000.0 * px).abs() < 3.0, "{width_m}");
    assert!((height_m - 700.0 * px).abs() < 3.0, "{height_m}");
    assert!(((b[0] + b[2]) / 2.0 - 11.64).abs() < 1e-9);
}

#[test]
fn boxes_that_touch_overlap_and_boxes_apart_do_not() {
    let a = [11.0, 48.0, 11.1, 48.1];
    assert!(boxes_overlap(a, [11.05, 48.05, 11.2, 48.2]));
    assert!(boxes_overlap(a, [11.1, 48.1, 11.2, 48.2]));
    assert!(!boxes_overlap(a, [11.2, 48.0, 11.3, 48.1]));
    assert!(!boxes_overlap(a, [11.0, 48.2, 11.1, 48.3]));
}

use serde_json::json;

#[test]
fn the_last_plan_is_remembered_and_forgotten_by_name() {
    let dir = std::env::temp_dir().join(format!("corvus-last-plan-{}", std::process::id()));
    let file = dir.join("last.txt");
    assert_eq!(last::read_at(&file), None, "nothing yet means start empty");
    last::remember_at(&file, "Field A");
    assert_eq!(last::read_at(&file).as_deref(), Some("Field A"));
    last::forget_at(&file, "Field B");
    assert_eq!(
        last::read_at(&file).as_deref(),
        Some("Field A"),
        "another plan's removal leaves it"
    );
    last::forget_at(&file, "Field A");
    assert_eq!(last::read_at(&file), None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_finished_shape_names_the_finish_button_for_a_finger() {
    assert_eq!(
        ready_hint(true),
        "Press FINISH to cover it. Tap for more points, hold the map to take one back."
    );
    assert!(ready_hint(false).starts_with("Enter or a double click"));
}

#[test]
fn the_stacked_map_takes_55_percent_of_the_window_and_never_under_320() {
    assert_eq!(stacked_map_height(1000.0), 550.0);
    assert_eq!(stacked_map_height(500.0), 320.0);
}

fn page() -> MissionPage {
    MissionPage::new(Server::local(1), Value::Null)
}

fn click(p: &mut MissionPage, lon: f64, lat: f64) {
    p.apply_map(Out {
        clicked: Some([lon, lat]),
        ..Out::default()
    });
}

#[test]
fn the_start_then_points_then_one_ending_is_the_order_a_plan_is_drawn_in() {
    let mut p = page();
    click(&mut p, 11.0, 48.0);
    assert_eq!(p.plan.items.len(), 0, "SELECT places nothing");
    p.press_tool(Tool::Item(Kind::Waypoint));
    assert_eq!(p.tool, Tool::Select, "no start yet, so the tool stays unarmed");
    assert_eq!(p.hint, "Set the start first. The mission begins there.");
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    assert_eq!(p.plan.items[0].kind, Kind::Takeoff);
    assert_eq!(p.tool, Tool::Item(Kind::Waypoint), "the route follows the start");
    click(&mut p, 11.001, 48.0);
    click(&mut p, 11.002, 48.0);
    p.press_tool(Tool::Item(Kind::Land));
    assert_eq!(p.tool, Tool::Item(Kind::Land));
    click(&mut p, 11.003, 48.0);
    assert_eq!(
        p.tool,
        Tool::Select,
        "a landing ends the mission: no second one to place"
    );
    let kinds: Vec<Kind> = p.plan.items.iter().map(|i| i.kind).collect();
    assert_eq!(kinds, [Kind::Takeoff, Kind::Waypoint, Kind::Waypoint, Kind::Land]);
    p.press_tool(Tool::Item(Kind::Rtl));
    assert_eq!(p.plan.items.len(), 4, "the mission already ends with a landing");
}

#[test]
fn a_return_is_added_at_once_and_names_no_place() {
    let mut p = page();
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    p.press_tool(Tool::Item(Kind::Rtl));
    assert_eq!(p.plan.items.last().map(|i| i.kind), Some(Kind::Rtl));
    assert_eq!(p.tool, Tool::Select);
}

#[test]
fn an_area_becomes_waypoints_before_the_ending_in_one_group() {
    let mut p = page();
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    p.press_tool(Tool::Item(Kind::Rtl));
    p.press_tool(Tool::Pattern);
    assert!(p.tools.pattern_pinned, "PATTERN opens its card and arms nothing");
    assert_eq!(p.tool, Tool::Select);
    p.set_tool(Tool::Shape(Shape::Area));
    for c in [[11.0, 48.0], [11.002, 48.0], [11.002, 48.001]] {
        click(&mut p, c[0], c[1]);
    }
    assert!(p.pattern_ready());
    assert_eq!(
        p.hint,
        "Enter or a double click covers it. Click for more points, right click undoes one."
    );
    p.apply_map(Out {
        double_clicked: true,
        ..Out::default()
    });
    assert_eq!(p.tool, Tool::Select);
    let waypoints: Vec<_> = p.plan.items.iter().filter(|i| i.kind == Kind::Waypoint).collect();
    assert!(waypoints.len() >= 4);
    assert!(waypoints.iter().all(|w| w.pattern == Some(1)));
    assert_eq!(
        p.plan.items.last().map(|i| i.kind),
        Some(Kind::Rtl),
        "the ending stays last"
    );
    assert!(p
        .hint
        .starts_with(&format!("Pattern added: {} points.", waypoints.len())));
}

#[test]
fn a_circle_is_one_item_as_wide_as_the_second_click() {
    let mut p = page();
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    p.set_tool(Tool::Shape(Shape::Circle));
    click(&mut p, 11.0, 48.0);
    assert!(p.plan.items.len() == 1, "one click is not a circle");
    click(&mut p, 11.0, 48.0 + 80.0 / 111_195.0);
    let c = p.plan.items.last().expect("the circle");
    assert_eq!(c.kind, Kind::LoiterTurns);
    assert_eq!((c.radius, c.turns, c.direction), (80.0, 1.0, 1.0));
}

#[test]
fn a_circle_never_gets_smaller_than_ten_metres() {
    let mut p = page();
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    p.set_tool(Tool::Shape(Shape::Circle));
    click(&mut p, 11.0, 48.0);
    click(&mut p, 11.0, 48.0 + 2.0 / 111_195.0);
    assert_eq!(p.plan.items.last().map(|i| i.radius), Some(10.0));
}

#[test]
fn a_right_click_takes_back_the_last_corner_then_the_newest_item() {
    let mut p = page();
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    click(&mut p, 11.001, 48.0);
    click(&mut p, 11.002, 48.0);
    p.set_tool(Tool::Shape(Shape::Corridor));
    click(&mut p, 11.0, 48.001);
    let right = || Out {
        secondary: Some([0.0, 0.0]),
        ..Out::default()
    };
    p.apply_map(right());
    assert!(p.draft.pts.is_empty());
    assert_eq!(p.plan.items.len(), 3, "the corner went, not an item");
    p.set_tool(Tool::Select);
    p.apply_map(right());
    assert_eq!(p.plan.items.len(), 2, "the newest item goes");
    p.apply_map(right());
    p.apply_map(right());
    assert!(p.plan.items.is_empty() && p.plan.home.is_none() || p.plan.home.is_some());
}

#[test]
fn dragging_a_point_moves_it_and_a_right_click_on_it_removes_it() {
    let mut p = page();
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    click(&mut p, 11.001, 48.0);
    let id = p.plan.items[1].id;
    p.apply_map(Out {
        moved: Some((id, [11.005, 48.002])),
        ..Out::default()
    });
    assert_eq!((p.plan.items[1].lon, p.plan.items[1].lat), (11.005, 48.002));
    p.apply_map(Out {
        remove: Some(id),
        ..Out::default()
    });
    assert_eq!(p.plan.items.len(), 1);
}

#[test]
fn dragging_the_start_lets_go_of_the_aircraft_and_takes_the_takeoff_along() {
    let mut p = page();
    p.plan.start_at_vehicle = true;
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    p.plan.start_at_vehicle = true;
    p.apply_map(Out {
        start_moved: Some([11.01, 48.01]),
        ..Out::default()
    });
    assert!(!p.plan.start_at_vehicle);
    assert_eq!((p.plan.items[0].lon, p.plan.items[0].lat), (11.01, 48.01));
    assert_eq!(
        p.hint,
        "The start stays where you put it. AIRCRAFT ties it to the aircraft again."
    );
}

#[test]
fn the_radius_grip_sets_the_radius_in_whole_metres() {
    let mut p = page();
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    p.set_tool(Tool::Shape(Shape::Circle));
    click(&mut p, 11.0, 48.0);
    click(&mut p, 11.0, 48.0 + 50.0 / 111_195.0);
    let id = p.plan.items.last().map(|i| i.id).expect("the circle");
    p.apply_map(Out {
        radius: Some((id, 123.0)),
        ..Out::default()
    });
    assert_eq!(p.plan.items.last().map(|i| i.radius), Some(123.0));
}

#[test]
fn a_pattern_moves_as_one_by_the_offset_of_its_grip() {
    let mut p = page();
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    p.set_tool(Tool::Shape(Shape::Corridor));
    click(&mut p, 11.0, 48.0);
    click(&mut p, 11.002, 48.0);
    p.apply_map(Out {
        double_clicked: true,
        ..Out::default()
    });
    let group: Vec<(u32, f64)> = p
        .plan
        .items
        .iter()
        .filter(|i| i.pattern.is_some())
        .map(|i| (i.id, i.lat))
        .collect();
    assert!(group.len() >= 2);
    p.plan.selected = Some(group[0].0);
    p.apply_map(Out {
        pattern_grab: Some([11.001, 48.0]),
        ..Out::default()
    });
    p.apply_map(Out {
        pattern_to: Some([11.001, 48.001]),
        ..Out::default()
    });
    for (id, lat) in &group {
        let now = p.plan.items.iter().find(|i| i.id == *id).map(|i| i.lat).unwrap_or(0.0);
        assert!((now - lat - 0.001).abs() < 1e-9, "{now} {lat}");
    }
}

#[test]
fn the_aircraft_button_puts_the_start_where_the_aircraft_stands() {
    let mut p = page();
    let state = json!({"connected": true, "armed": false, "position": [11.64, 48.07], "vehicle_type": "QUADROTOR"});
    p.link = Link {
        connected: true,
        start: vehicle_start(&state),
    };
    p.press_tool(Tool::VehicleStart);
    let h = p.plan.home.expect("a start");
    assert_eq!((h.lon, h.lat), (11.64, 48.07));
    assert!(p.plan.start_at_vehicle);
    p.press_tool(Tool::VehicleStart);
    assert!(!p.plan.start_at_vehicle, "pressed again it lets go");
}

#[test]
fn the_start_of_an_armed_aircraft_is_its_home() {
    let armed = json!({"connected": true, "armed": true, "position": [11.7, 48.2], "home": [11.6, 48.1]});
    assert_eq!(vehicle_start(&armed), Some((11.6, 48.1)));
    let no_home = json!({"connected": true, "armed": true, "position": [11.7, 48.2], "home": [0, 0]});
    assert_eq!(vehicle_start(&no_home), Some((11.7, 48.2)));
    assert_eq!(vehicle_start(&json!({"connected": true, "position": [0, 0]})), None);
    assert_eq!(
        vehicle_start(&json!({"connected": false, "position": [11.7, 48.2]})),
        None
    );
    assert_eq!(
        vehicle_start(&json!({"connected": true, "position": [11.7, 148.2]})),
        None
    );
}

#[test]
fn the_bounds_include_an_orbits_ring() {
    let mut p = page();
    p.press_tool(Tool::Home);
    click(&mut p, 11.0, 48.0);
    assert_eq!(p.bounds(), Some([11.0, 48.0, 11.0, 48.0]));
    p.set_tool(Tool::Shape(Shape::Circle));
    click(&mut p, 11.001, 48.0);
    click(&mut p, 11.001, 48.0 + 100.0 / 111_195.0);
    let [w, s, e, n] = p.bounds().expect("bounds");
    assert!(w < 11.0 && e > 11.001 && s < 48.0 && n > 48.0, "{w} {s} {e} {n}");
    assert!(n - 48.0 > 0.0008, "the ring's 100 m reach north of the centre");
}
