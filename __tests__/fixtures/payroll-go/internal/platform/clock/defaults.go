package clock

var (
	DefaultZone = "UTC"
	fallbackZone = DefaultZone
)

func Zone() string {
	var (
		DefaultZone = "local"
	)
	return DefaultZone
}
