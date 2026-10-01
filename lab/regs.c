typedef struct List {
	int length;
	int *elems;
} List;

extern List *list_delete_nth(List *list, int n);
extern const char *lookup_name(int id);

List *list_delete_value(List *list, int value)
{
	for (int i = 0; i < list->length; i++)
		if (list->elems[i] == value)
			return list_delete_nth(list, i);
	return list;
}

const char *first_match(const int *ids, int n, int lo, int hi)
{
	for (int i = 0; i < n; i++)
		if (ids[i] >= lo && ids[i] <= hi)
			return lookup_name(ids[i]);
	return 0;
}

int count_between(const int *v, int n, int lo, int hi, int step)
{
	int c = 0;

	for (int i = 0; i < n; i += step)
		if (v[i] >= lo && v[i] <= hi)
			c++;
	return c;
}
